"""Native iTerm2 acceptance using only owned synthetic sessions and loopback."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.dont_write_bytecode = True
sys.path.insert(0, '/Users/zzq/Develops/minicore-tui/scripts')
from stage7_loopback_model import completed_event

BASE = Path('/tmp/minicore-followups.AdCugX/accepted')
ROOT = Path(tempfile.mkdtemp(prefix='minicore-session-native-'))
WORK = ROOT / 'workspace'
OUT = ROOT / 'evidence'
CONF = ROOT / 'config'
for path in [WORK, OUT, CONF / 'prompts']:
    path.mkdir(parents=True)
PROFILE = os.environ.get('NATIVE_PROFILE', 'debug')
TUI = BASE / 'artifacts' / ('minicore-tui-' + PROFILE)
AGENT = BASE / 'artifacts' / ('minicore-agent-' + PROFILE)
release = threading.Event()
requests = []

class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        serialized = json.dumps(body)
        number = len(requests)
        requests.append({'number': number, 'model': body.get('model'),
                         'file_prompt_present': 'NATIVE_PROMPT_ORIGINAL' in serialized,
                         'changed_prompt_absent': 'NATIVE_PROMPT_CHANGED' not in serialized})
        if number == 1 and not release.wait(40):
            raise RuntimeError('owned busy response was not released')
        text = 'BUSY_DONE' if number == 1 else 'PROMPT_FILE_OK'
        message = {'type': 'message', 'id': f'msg_{number}', 'role': 'assistant',
                   'status': 'completed', 'content': [{'type': 'output_text', 'text': text, 'annotations': []}]}
        events = [
            {'type': 'response.output_item.added', 'output_index': 0,
             'item': dict(message, content=[], status='in_progress')},
            {'type': 'response.output_text.delta', 'output_index': 0, 'content_index': 0,
             'item_id': message['id'], 'delta': text},
            {'type': 'response.output_item.done', 'output_index': 0, 'item': message},
            completed_event(0, [message]),
        ]
        data = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(data)

server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
(CONF / 'prompts/system.md').write_text('NATIVE_PROMPT_ORIGINAL\nSynthetic fixture only.\n')
config = f'''data_dir = {json.dumps(str(ROOT / 'data'))}
default_profile = "coding"
[profiles.coding]
model = "verify-a"
reasoning = "high"
system_prompt = {{ file = "prompts/system.md" }}
tools = []
max_tool_rounds = 4
approval = "auto"
'''
for model in ['verify-a', 'verify-b', 'verify-c']:
    config += f'''
[models.{model}]
provider = "open_ai_responses"
model = "{model}"
base_url = "http://127.0.0.1:{server.server_port}/v1"
api_key_env = "MINICORE_NATIVE_SESSION_KEY"
physical_context_window = 32000
output_budget_tokens = 4096
safety_margin_tokens = 1000
supported_reasoning = ["high", "max"]
supports_tools = true
request_timeout_seconds = 60
'''
(CONF / 'agent.toml').write_text(config)

def osa(script):
    return subprocess.check_output(['osascript', '-e', script], text=True).rstrip('\n')

window = int(osa('tell application "iTerm2"\nactivate\nset w to (create window with default profile command "/bin/sh")\nreturn id of w\nend tell'))

def send(text):
    value = ' & '.join(f'(character id {ord(char)})' for char in text)
    osa(f'tell application "iTerm2" to tell current session of window id {window} to write text ({value}) newline NO')
    if text == '\x1b':
        time.sleep(0.2)

def screen():
    rows = int(osa(f'tell application "iTerm2" to get rows of current session of window id {window}'))
    contents = osa(f'tell application "iTerm2" to get contents of current session of window id {window}')
    return '\n'.join(contents.splitlines()[-rows:])

def wait(predicate, label, seconds=15):
    deadline = time.monotonic() + seconds
    text = ''
    while time.monotonic() < deadline:
        text = screen()
        if predicate(text):
            return text
        time.sleep(0.1)
    (OUT / 'failure-screen.txt').write_text(text)
    raise AssertionError(label)

def has(*words):
    return lambda text: all(word in text for word in words)

def capture(name):
    (OUT / (name + '.txt')).write_text(screen())

def size(columns, rows):
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to {columns}\nset rows to {rows}\nend tell')
    time.sleep(0.15)

def click_label(label, twice=False):
    text = wait(has(label), 'clickable label ' + label)
    row, line = next((i + 1, line) for i, line in enumerate(text.splitlines()) if label in line)
    col = line.index(label) + 2
    for _ in range(2 if twice else 1):
        send(f'\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m')
        time.sleep(0.12)

checks = []
result = {'profile': PROFILE, 'window_id': window, 'evidence_dir': str(OUT),
          'tui_sha256': hashlib.sha256(TUI.read_bytes()).hexdigest(),
          'agent_sha256': hashlib.sha256(AGENT.read_bytes()).hexdigest(),
          'pixel_screenshots': 'UNVERIFIED; actual iTerm2 screen text/input only'}
try:
    size(100, 32)
    time.sleep(0.3)
    command = f'cd {shlex.quote(str(WORK))} && env MINICORE_NATIVE_SESSION_KEY=placeholder {shlex.quote(str(TUI))} --agent-bin {shlex.quote(str(AGENT))} --agent-config {shlex.quote(str(CONF / "agent.toml"))} --workspace {shlex.quote(str(WORK))} --theme dark; rc=$?; printf "\\nSESSION_NATIVE_EXIT=%s\\n" "$rc"'
    send('/bin/sh -c ' + shlex.quote(command) + '\r')
    wait(has('Create session'), 'new form')
    send('\t\t\t\tNative Original\t\r')
    wait(has('ready', 'verify a'), 'created session')
    (CONF / 'prompts/system.md').write_text('NATIVE_PROMPT_CHANGED\n')
    send('check startup prompt snapshot\r')
    wait(has('PROMPT_FILE_OK', 'ready'), 'file prompt turn')
    assert requests[0]['file_prompt_present'] and requests[0]['changed_prompt_absent']
    checks.append('relative prompt path resolves outside cwd and startup snapshot survives file edit')
    send('hold while renaming\r')
    wait(lambda text: len(requests) == 2 and 'working' in text, 'busy request')
    send('\x12')
    wait(has('Select session', 'Native Original'), 'Session browser')
    click_label('F2 Rename')
    wait(has('Title:'), 'rename form')
    send('\x15Renamed During Work 中文\r')
    wait(has('Select session', 'Renamed During Work', 'F2 Rename'), 'busy rename ACK')
    click_label('Del Delete')
    time.sleep(0.2)
    assert 'Permanently delete' not in screen() and 'Close before deleting' not in screen()
    capture('01-busy-rename-delete-blocked')
    checks.append('mouse rename works during a live request; delete does not cancel or close it')
    release.set()
    send('\x1b')
    wait(has('BUSY_DONE', 'ready'), 'busy completed')
    send('\x0e')
    wait(has('Create session'), 'second new form')
    send('\t\t\t\tNative Survivor\t\r')
    wait(has('ready'), 'second created')
    send('\x12')
    wait(has('Native Survivor', 'Renamed During Work'), 'two sessions')
    send('Renamed During Work')
    wait(lambda text: '→ Renamed During Work' in text and 'Native Survivor' not in text, 'filter selects the visible original')
    capture('01b-filter-selected-original')
    click_label('F5 Refresh')
    time.sleep(0.2)
    wait(has('→ Renamed During Work'), 'refresh keeps filtered selected identity')
    capture('01c-refreshed-selected-original')
    click_label('Del Delete')
    wait(lambda text: 'Close before deleting?' in text and 'Renamed During Work' in text and 'Native Survivor' not in text, 'loaded target requires close first')
    send('\r')
    wait(has('Permanently delete this session?'), 'separate delete confirmation')
    size(60, 16)
    wait(has('Permanently delete', '[ Cancel ]', '[ Delete ]', 'Renamed During'), '60x16 confirmation controls')
    capture('02-delete-confirm-60x16')
    send('\r')
    wait(has('Select session', 'Renamed During Work', 'Del Delete'), 'default Enter cancels')
    click_label('F5 Refresh')
    wait(has('Renamed During Work'), 'default cancel retains durable session')
    checks.append('selected non-active target closes separately; default Cancel never deletes')
    click_label('Del Delete')
    wait(has('Permanently delete'), 'closed target confirmation')
    click_label('[ Delete ]')
    wait(has('No matching sessions'), 'explicit mouse delete ACK')
    send('\x15')
    wait(has('Native Survivor'), 'other session survives')
    capture('03-survivor-60x16')
    checks.append('explicit delete affects only selected session; another session remains')
    size(100, 32)
    send('\r')
    wait(has('ready'), 'return survivor')
    send('/model\r')
    wait(has('Select model', 'verify-b'), 'shared model selector')
    click_label('verify-b', twice=True)
    wait(has('ready', 'verify b'), 'model mouse selection acknowledged')
    send('/reasoning\r')
    wait(has('Select reasoning', 'max'), 'shared reasoning selector')
    click_label('max', twice=True)
    wait(has('ready', '· max ·'), 'reasoning mouse selection acknowledged')
    send('/new\r')
    wait(has('New session'), 'shared new form')
    send('\t\r')
    wait(has('Select profile', 'coding'), 'shared profile selector')
    capture('04-profile-panel')
    send('\x1b')
    wait(has('New session'), 'profile escape returns to new form')
    time.sleep(0.15)
    send('\x1b')
    wait(lambda text: 'New session' not in text and 'Select profile' not in text and 'ready' in text, 'new form escape returns to composer')
    time.sleep(0.15)
    send('/help\r')
    wait(has('Help'), 'shared help panel')
    send('\x1b[F\x1b[H\x1b[6~\x1b[5~')
    capture('05-help-panel')
    send('\x1b')
    send('/logs\r')
    wait(has('Agent logs'), 'shared logs panel')
    send('\x1b[F\x1b[H')
    capture('06-logs-panel')
    send('\x1b')
    checks.append('model/reasoning mouse selection and profile/new/help/logs shared panels work')
    send('verify selected model and prompt snapshot\r')
    wait(has('PROMPT_FILE_OK', 'ready'), 'survivor provider turn')
    assert requests[-1]['model'] == 'verify-b'
    assert all(req['file_prompt_present'] and req['changed_prompt_absent'] for req in requests)
    send('\x03')
    time.sleep(0.15)
    send('\x03')
    wait(has('SESSION_NATIVE_EXIT=0'), 'clean TUI exit')
    result['status'] = 'PASS'
except Exception as error:
    result['status'] = 'FAIL'
    result['error'] = str(error)
    raise
finally:
    release.set()
    if result.get('status') != 'PASS':
        for _ in range(3):
            try:
                send('\x03')
                time.sleep(0.15)
            except Exception:
                break
    server.shutdown()
    server.server_close()
    result.update(checks=checks, requests=requests)
    (OUT / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    if result.get('status') == 'PASS':
        osa(f'tell application "iTerm2" to close window id {window}')
