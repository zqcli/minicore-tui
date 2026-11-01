"""Fixture/gate/cleanup tests; these do not claim canonical chain acceptance."""

import faulthandler
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from process_support import assert_group_exited, cleanup_group, register_group
from real_chain_load import Chain, Plan, Screen, fixture_hash, process_cpu_ticks


class HarnessTests(unittest.TestCase):
    def test_tool_loop_is_three_real_requests_not_three_turns(self):
        plan = Plan(tool_every=True)
        self.assertEqual([True, True, True, False] * 2,
                         [plan.is_tool(index) for index in range(8)])
        self.assertEqual([3, 7], [plan.text_index(index) for index in range(2)])

    def test_approvals_have_a_distinct_single_tool_plan(self):
        plan = Plan(tool_every=True, tool_steps=1, tool_name="write")
        self.assertEqual([True, False, True, False], [plan.is_tool(index) for index in range(4)])

    def test_generated_hash_is_not_whitespace_collapsed(self):
        self.assertNotEqual(fixture_hash(["a  b"]), fixture_hash(["a b"]))

    @unittest.skipUnless(hasattr(os, 'openpty'), 'Unix PTY')
    def test_exited_terminal_is_drained_before_paced_restore_is_judged(self):
        master, slave = os.openpty()
        chain = Chain.__new__(Chain)
        chain.master = master
        chain.screen = Screen()
        chain.screen_only = True
        chain.pumps = []
        chain.provider = SimpleNamespace(check=lambda: None)
        chain.sampler = SimpleNamespace(sample=lambda: None)
        chain.process = SimpleNamespace(poll=lambda: 0, returncode=0)
        try:
            # macOS can discard unread PTY output on the last writer close.
            os.write(slave, b'\x1b[?1049l')
            self.assertFalse(chain.screen.saw_restore)
            chain.wait_for('terminal restore', lambda: chain.screen.saw_restore, timeout=0)
            self.assertTrue(chain.screen.saw_restore)
            with self.assertRaisesRegex(AssertionError, 'TUI exited during missing actual marker'):
                chain.wait_for('missing actual marker', lambda: False, timeout=0)
        finally:
            os.close(master)
            if slave is not None:
                os.close(slave)

    def test_cpu_ticks_read_actual_user_and_system_fields_with_spaced_process_name(self):
        fields = ['S'] + ['0'] * 30
        fields[11], fields[12] = '123', '45'
        with patch('pathlib.Path.read_text', return_value='7 (name with spaces) ' + ' '.join(fields)):
            self.assertEqual(process_cpu_ticks(7), 168)

    def test_unavailable_process_cpu_is_not_fabricated_zero(self):
        with patch('pathlib.Path.read_text', side_effect=FileNotFoundError):
            self.assertIsNone(process_cpu_ticks(7))

    def test_failed_enter_closes_provider_and_temp_directory(self):
        faulthandler.dump_traceback_later(5, repeat=True)
        self.addCleanup(faulthandler.cancel_dump_traceback_later)
        with tempfile.TemporaryDirectory() as root:
            chain = Chain("/does/not/exist", root, Plan(), screen_only=True)
            temporary = chain.temporary.name
            with self.assertRaises(AssertionError):
                with chain:
                    self.fail("unreachable")
            self.assertFalse(Path(temporary).exists())
            self.assertFalse(chain.provider.thread.is_alive())

    def test_pty_launcher_executes_with_a_controlling_terminal_and_owned_group(self):
        master, slave = os.openpty()
        code = "import os; fd=os.open('/dev/tty', os.O_RDWR); print(os.getsid(0)==os.getpid(), os.tcgetpgrp(fd)==os.getpgrp())"
        process = None
        try:
            process = subprocess.Popen(
                [sys.executable, str(Path(__file__).with_name('process_support.py')),
                 sys.executable, '-c', code], start_new_session=True, stdin=slave,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            output, errors = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 0, errors)
            self.assertEqual(output, 'True True\n')
        finally:
            cleanup_group(process)
            os.close(master)
            os.close(slave)

    def test_darwin_zombie_group_requires_actual_disappearance(self):
        with patch('process_support.sys.platform', 'darwin'), \
             patch('process_support.os.kill', side_effect=ProcessLookupError), \
             patch('process_support.os.killpg', side_effect=[PermissionError, ProcessLookupError]) as probe, \
             patch('process_support.time.sleep'):
            assert_group_exited(123, 124)
            self.assertEqual(probe.call_count, 2)

        with patch('process_support.sys.platform', 'darwin'), \
             patch('process_support.os.kill', side_effect=ProcessLookupError), \
             patch('process_support.os.killpg', side_effect=PermissionError):
            with self.assertRaisesRegex(AssertionError, 'TUI process group remained'):
                assert_group_exited(123, 124, timeout=0)

    @unittest.skipUnless(hasattr(signal, "SIGALRM"), "Unix helper signals")
    def test_helper_termination_runs_cleanup_and_exits_unsuccessfully(self):
        code = '''import signal
from process_support import run_bounded
def main():
    try:
        print("ready", flush=True)
        signal.pause()
    finally:
        print("cleanup", flush=True)
run_bounded(main, 30, "fixture")
'''
        process = subprocess.Popen(
            [sys.executable, "-c", code], cwd=Path(__file__).parent,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        try:
            self.assertEqual(process.stdout.readline().strip(), "ready")
            process.terminate()
            output, error = process.communicate(timeout=5)
            self.assertEqual(output, "cleanup\n")
            self.assertEqual(process.returncode, 1)
            self.assertIn("fixture: TimeoutError:", error)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)

    @unittest.skipUnless(hasattr(os, "killpg"), "Unix process groups")
    def test_timeout_cleanup_kills_stubborn_group_and_records_pid(self):
        with tempfile.TemporaryDirectory() as root:
            registry = Path(root) / "groups"
            process = subprocess.Popen(
                [sys.executable, "-c", "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready', flush=True); time.sleep(60)"],
                start_new_session=True, stdout=subprocess.PIPE, text=True,
            )
            previous = os.environ.get("MINICORE_E2E_PROCESS_REGISTRY")
            try:
                self.assertEqual(process.stdout.readline().strip(), "ready")
                os.environ["MINICORE_E2E_PROCESS_REGISTRY"] = str(registry)
                register_group(process)
                self.assertEqual(registry.read_text().strip(), str(process.pid))
                with self.assertRaises(AssertionError):
                    assert_group_exited(process.pid, process.pid, timeout=0)
                cleanup_group(process)
                assert_group_exited(process.pid, process.pid)
                self.assertEqual(process.returncode, -signal.SIGKILL)
                with self.assertRaises(ProcessLookupError):
                    os.killpg(process.pid, 0)
            finally:
                cleanup_group(process)
                process.stdout.close()
                if previous is None:
                    os.environ.pop("MINICORE_E2E_PROCESS_REGISTRY", None)
                else:
                    os.environ["MINICORE_E2E_PROCESS_REGISTRY"] = previous


if __name__ == "__main__":
    unittest.main(verbosity=2)
