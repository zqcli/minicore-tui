"""Bounded helper execution and cleanup of PTY process groups."""

import os
import signal
import subprocess
import sys
import time


def run_bounded(main, seconds, label):
    def expired(_signum, _frame):
        raise TimeoutError("helper exceeded its outer deadline")

    signal.signal(signal.SIGALRM, expired)
    signal.signal(signal.SIGTERM, expired)
    signal.alarm(seconds)
    try:
        main()
    except Exception as error:
        print(f"{label}: {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
    finally:
        signal.alarm(0)


def register_group(process):
    path = os.environ.get("MINICORE_E2E_PROCESS_REGISTRY")
    if path:
        with open(path, "a", encoding="ascii") as handle:
            handle.write(f"{process.pid}\n")
            handle.flush()


def assert_group_exited(group_id, agent_pid, timeout=5):
    deadline = time.monotonic() + timeout
    while True:
        alive = []
        for name, probe, pid in (("Agent child", os.kill, agent_pid),
                                 ("TUI process group", os.killpg, group_id)):
            try:
                probe(pid, 0)
            except ProcessLookupError:
                continue
            alive.append(name)
        if not alive:
            return
        if time.monotonic() >= deadline:
            raise AssertionError(", ".join(alive) + " remained after bounded exit wait")
        # Poll the process-exit condition, not application/event scheduling.
        time.sleep(0.01)


def cleanup_group(process):
    if process is None:
        return
    # A naturally exited leader can still have live descendants in its group.
    try:
        os.killpg(process.pid, signal.SIGCONT)
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired as error:
        raise AssertionError("could not reap PTY process within cleanup bound") from error
