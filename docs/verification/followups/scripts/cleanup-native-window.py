"""Gracefully retire the single failed window created by this acceptance run."""
from pathlib import Path
import json
import shlex
import subprocess
import time

window = 74470
root = Path('/private/var/folders/zw/gcpf3t91243dhn6z3bb278s00000gn/T/minicore-native-followups-z79x6pl1')
config = root / 'config/agent.toml'
tui = Path('/tmp/minicore-followups.AdCugX/accepted/artifacts/minicore-tui-debug').resolve()

def osa(script):
    return subprocess.check_output(['osascript', '-e', script], text=True).strip()

owned = []
for row in subprocess.check_output(['ps', '-ww', '-axo', 'pid=,command='], text=True).splitlines():
    fields = row.strip().split(None, 1)
    if len(fields) != 2 or str(config) not in fields[1]:
        continue
    args = shlex.split(fields[1])
    if args and Path(args[0]).resolve() == tui and '--agent-config' in args:
        assert Path(args[args.index('--agent-config') + 1]) == config
        owned.append(int(fields[0]))
assert len(owned) == 1, f'expected one owned TUI, found {owned}'
tty = osa(f'tell application "iTerm2" to get tty of current session of window id {window}')
process_tty = subprocess.check_output(['ps', '-p', str(owned[0]), '-o', 'tty='], text=True).strip()
assert tty == '/dev/' + process_tty, (tty, process_tty)
for _ in range(3):
    osa(f'tell application "iTerm2" to tell current session of window id {window} to write text (character id 3) newline NO')
    time.sleep(0.2)
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    text = osa(f'tell application "iTerm2" to get contents of current session of window id {window}')
    if 'NATIVE_FOLLOWUPS_EXIT=0' in text:
        osa(f'tell application "iTerm2" to close window id {window}')
        print(json.dumps({'window': window, 'owned_tui_pid': owned[0], 'tty': tty, 'exit': 0, 'closed': True}))
        break
    time.sleep(0.1)
else:
    raise RuntimeError('owned TUI exit not confirmed; window left open')
