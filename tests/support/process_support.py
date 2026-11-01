"""Bounded helper execution and cleanup of PTY process groups."""

import fcntl
import os
import signal
import subprocess
import sys
import termios
import time
import traceback


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
        traceback.print_exc()
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
            except PermissionError:
                if sys.platform != "darwin" or probe is not os.killpg:
                    raise
                # Darwin can report EPERM while a group contains only zombies.
                # It remains present until the probe reports ESRCH.
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
    process.poll()
    # A naturally exited leader can still have live descendants in its group.
    signal_group(process.pid, signal.SIGCONT)
    signal_group(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        pass
    signal_group(process.pid, signal.SIGKILL)
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired as error:
        raise AssertionError("could not reap PTY process within cleanup bound") from error
    assert_group_exited(process.pid, process.pid)


def signal_group(group_id, signum):
    try:
        os.killpg(group_id, signum)
    except ProcessLookupError:
        pass
    except PermissionError:
        # Darwin may retain an unsignalable zombie group; cleanup still waits
        # for actual disappearance and never treats EPERM as an exit proof.
        if sys.platform != "darwin":
            raise


if __name__ == "__main__":
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    os.execvpe(sys.argv[1], sys.argv[1:], os.environ)
