"""Native iTerm2 verification. All Agent data and tool effects stay in this directory."""
import hashlib
import json
import ctypes
import os
from pathlib import Path
import shlex
import subprocess
import sys
import threading
import time
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.dont_write_bytecode = True
ROOT = Path(tempfile.mkdtemp(prefix='run-', dir=Path(__file__).resolve().parent))
REPO = Path('/Users/zzq/Develops/minicore-tui')
sys.path.insert(0, str(REPO / 'scripts'))
from stage7_loopback_model import completed_event

WORK = ROOT / 'workspace'
OUT = ROOT / 'evidence'
WORK.mkdir(exist_ok=True)
OUT.mkdir(exist_ok=True)
TUI = REPO / 'target/debug/minicore-tui'
AGENT = REPO.parent / 'minicore-agent/target/debug/minicore-agent'
requests = []
SCREEN_ACCESS = bool(ctypes.CDLL('/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics').CGPreflightScreenCaptureAccess())

class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        record = {
            'model': body.get('model'),
            'effort': body.get('reasoning', {}).get('effort'),
            'steer_present': 'steer-check' in json.dumps(body.get('input', [])),
        }
        requests.append(record)
        if len(requests) == 1:
            parts = ['Step one\n', 'Step two\n', 'Step three\n', 'Step four']
            reasoning = {'type': 'reasoning', 'id': 'rs_iterm', 'summary': [
                {'type': 'summary_text', 'text': ''.join(parts)}]}
            args = json.dumps({'command': "printf 'ITERM_TOOL_OK\\n' > tool-ran; while [ ! -f release-tool ]; do sleep 0.1; done; printf 'ITERM_TOOL_OK\\n'"})
            tool = {'type': 'function_call', 'call_id': 'iterm-bash', 'name': 'bash', 'arguments': args}
            events = [{'type': 'response.output_item.added', 'output_index': 0,
                       'item': {'type': 'reasoning', 'id': 'rs_iterm', 'summary': []}}]
            events += [{'type': 'response.reasoning_summary_text.delta', 'item_id': 'rs_iterm',
                        'output_index': 0, 'delta': text} for text in parts]
            events += [
                {'type': 'response.output_item.done', 'output_index': 0, 'item': reasoning},
                {'type': 'response.output_item.added', 'output_index': 1, 'item': dict(tool, arguments='')},
                {'type': 'response.function_call_arguments.delta', 'item_id': 'fc_iterm', 'output_index': 1, 'delta': args},
                {'type': 'response.function_call_arguments.done', 'item_id': 'fc_iterm', 'output_index': 1, 'arguments': args},
                {'type': 'response.output_item.done', 'output_index': 1, 'item': tool},
                completed_event(8, [reasoning, tool]),
            ]
        else:
            events = [{'type': 'response.output_text.delta', 'delta': 'ITERM_FINAL_OK: steering received.'}, completed_event()]
        payload = ''.join('data: ' + json.dumps(e) + '\n\n' for e in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def osa(script):
    return subprocess.check_output(['osascript', '-e', script], text=True).rstrip('\n')

server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
config = 'data_dir = ' + json.dumps(str(ROOT / 'agent-data')) + '\ndefault_profile = "coding"\n'
config += '[profiles.coding]\nmodel = "iterm-a"\nreasoning = "high"\nsystem_prompt = "Local native-terminal verification."\ntools = ["bash"]\nmax_tool_rounds = 4\napproval = "auto"\n'
for name in ('iterm-a', 'iterm-b'):
    config += f'''\n[models.{name}]
provider = "open_ai_responses"
model = "{name}"
base_url = "http://127.0.0.1:{server.server_port}/v1"
api_key_env = "MINICORE_ITERM_VERIFY_KEY"
physical_context_window = 32000
output_budget_tokens = 2048
safety_margin_tokens = 1000
supported_reasoning = ["high", "max"]
supports_tools = true
request_timeout_seconds = 30
'''
(ROOT / 'agent.toml').write_text(config)
window = int(osa('tell application "iTerm2"\nactivate\nset w to (create window with default profile)\nreturn id of w\nend tell'))


def send(text):
    value = ' & '.join(f'(character id {ord(c)})' for c in text)
    osa(f'tell application "iTerm2" to tell current session of window id {window} to write text ({value}) newline NO')


def screen():
    return osa(f'tell application "iTerm2" to tell current session of window id {window} to get contents')


def wait(predicate, label, seconds=12):
    deadline = time.monotonic() + seconds
    last = ''
    while time.monotonic() < deadline:
        last = screen()
        if predicate(last):
            return last
        time.sleep(0.1)
    (OUT / 'failure-screen.txt').write_text(last)
    raise AssertionError(f'{label}: timeout; last native screen in {OUT / "failure-screen.txt"}')


def has(*words):
    return lambda text: all(word in text for word in words)


def capture(name):
    text = screen()
    (OUT / f'{name}.txt').write_text(text)
    dimensions = osa(f'tell application "iTerm2" to tell current session of window id {window} to get {{columns, rows}}')
    osa(f'tell application "iTerm2"\nactivate\nselect window id {window}\nend tell')
    bounds = osa(f'tell application "iTerm2" to get bounds of window id {window}')
    x, y, right, bottom = [int(v.strip()) for v in bounds.split(',')]
    capture_exit = None
    if SCREEN_ACCESS:
        result = subprocess.run(['/usr/sbin/screencapture', '-x', '-t', 'png', '-R', f'{x},{y},{right-x},{bottom-y}', str(OUT / f'{name}.png')], capture_output=True)
        capture_exit = result.returncode
    return {'name': name, 'dimensions': dimensions, 'bounds': bounds, 'capture_exit': capture_exit}


def choose_reasoning(direction, target, status):
    send('/reasoning\r')
    wait(has('Select reasoning', 'max', 'high'), 'reasoning selector')
    send(direction + '\r')
    wait(has(f'· {target} ·', status), f'footer acknowledged {target}')

checks = []
captures = []
result = {'terminal': 'iTerm2 ' + osa('tell application "iTerm2" to get version'), 'window_id': window, 'evidence_dir': str(OUT), 'screen_capture_access': SCREEN_ACCESS, 'tui_sha256_at_start': hashlib.sha256(TUI.read_bytes()).hexdigest(), 'agent_sha256_at_start': hashlib.sha256(AGENT.read_bytes()).hexdigest()}
try:
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to 100\nset rows to 32\nset name to "MiniCore 0.2.3 verification"\nend tell')
    command = f'cd {shlex.quote(str(WORK))} && env MINICORE_ITERM_VERIFY_KEY=loopback-placeholder {shlex.quote(str(TUI))} --agent-bin {shlex.quote(str(AGENT))} --agent-config {shlex.quote(str(ROOT / "agent.toml"))} --workspace {shlex.quote(str(WORK))} --theme dark; rc=$?; printf "\\nVERIFY_EXIT=%s\\n" "$rc"'
    send('/bin/sh -c ' + shlex.quote(command) + '\r')
    wait(has('Create session'), 'new session')
    send('\t\t\t\t\t\r')
    wait(has('ctx', 'ready', 'iterm a'), 'active session')
    choose_reasoning('\x1b[B', 'max', 'ready')
    assert len(requests) == 0
    checks.append('idle reasoning ack updates footer before any provider request')
    captures.append(capture('01-idle-max'))
    send('spacing-check\r')
    wait(lambda text: (WORK / 'tool-ran').exists() and 'working' in text.lower(), 'real bash executing')
    assert requests == [{'model': 'iterm-a', 'effort': 'max', 'steer_present': False}]
    checks.append('real bash executes under auto approval, request carries literal max')
    captures.append(capture('02-thinking-bash-working'))
    choose_reasoning('\x1b[A', 'high', 'working')
    assert len(requests) == 1 and requests[0]['effort'] == 'max'
    checks.append('live reasoning ack updates footer while active request remains max')
    send('steer-check\r')
    wait(has('awaiting history', 'steer-check'), 'accepted steer User card')
    assert len(requests) == 1
    captures.append(capture('03-accepted-steer-before-completion'))
    checks.append('accepted steer is visible before original tool completes')
    (WORK / 'release-tool').touch()
    wait(has('ITERM_FINAL_OK', 'ready'), 'loop complete', seconds=20)
    final = wait(lambda text: 'ITERM_FINAL_OK' in text and 'awaiting history' not in text and 'queued' not in text, 'history reconciled')
    assert requests[1] == {'model': 'iterm-a', 'effort': 'high', 'steer_present': True}, requests
    assert final.count('steer-check') == 1, final
    checks.append('next request uses high and contains steer; final User card appears exactly once')
    captures.append(capture('04-reconciled-loop'))
    choose_reasoning('\x1b[B', 'max', 'ready')
    send('/model\r')
    wait(has('Select model', 'iterm-b'), 'model selector')
    send('\x1b[B\r')
    wait(has('iterm b · max ·', 'ready'), 'idle model footer immediately updated')
    assert len(requests) == 2
    captures.append(capture('05-idle-model-and-reasoning'))
    checks.append('idle model and reasoning changes update footer with old history present, no turn sent')
    for cols, rows in ((80, 24), (60, 16)):
        osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to {cols}\nset rows to {rows}\nend tell')
        time.sleep(0.3)
        captures.append(capture(f'06-resize-{cols}x{rows}'))
    send('\x03')
    time.sleep(0.2)
    send('\x03')
    wait(has('VERIFY_EXIT=0'), 'clean native terminal shutdown')
    checks.append('TUI exits zero and returns to native iTerm shell')
    assert result['tui_sha256_at_start'] == hashlib.sha256(TUI.read_bytes()).hexdigest(), 'TUI binary changed during verification'
    assert result['agent_sha256_at_start'] == hashlib.sha256(AGENT.read_bytes()).hexdigest(), 'Agent binary changed during verification'
    result['status'] = 'PASS'
except Exception as error:
    result['status'] = 'FAIL'
    result['error'] = str(error)
    raise
finally:
    (WORK / 'release-tool').touch()
    if result.get('status') != 'PASS':
        send('\x03')
        time.sleep(0.2)
        send('\x03')
    server.shutdown()
    server.server_close()
    result.update(checks=checks, captures=captures, requests=requests,
                  tui_version=subprocess.check_output([str(TUI), '--version'], text=True).strip(),
                  tui_sha256=hashlib.sha256(TUI.read_bytes()).hexdigest(),
                  agent_sha256=hashlib.sha256(AGENT.read_bytes()).hexdigest(),
                  note='Native iTerm screen text assertions. PNG visual confirmation is a separate parent check. Synthetic loopback only; no external provider request.')
    (OUT / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
