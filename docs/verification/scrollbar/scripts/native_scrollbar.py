"""Native iTerm2 + real Agent, synthetic Store and loopback provider only."""
import hashlib, json, os, re, shlex, subprocess, sys, tempfile, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
sys.dont_write_bytecode = True
sys.path.insert(0, '/Users/zzq/Develops/minicore-tui/scripts')
from stage7_loopback_model import completed_event
ROOT = Path(tempfile.mkdtemp(prefix='minicore-native-scrollbar-'))
WORK = ROOT / 'workspace'; WORK.mkdir()
OUT = ROOT / 'evidence'; OUT.mkdir()
TUI = Path(sys.argv[1]).resolve()
AGENT = Path('/Users/zzq/Develops/minicore-agent/target/debug/minicore-agent')
requests = []
class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append({'model': body['model']})
        text = '\n\n'.join(f'SCROLL_ROW_{i:03} — synthetic 中文 e\u0301 example' for i in range(120))
        message = {'type':'message','id':'msg_scrollbar','role':'assistant','status':'completed','content':[{'type':'output_text','text':text,'annotations':[]}]}
        events = [
          {'type':'response.output_item.added','output_index':0,'item':dict(message,status='in_progress',content=[])},
          {'type':'response.output_text.delta','output_index':0,'content_index':0,'item_id':'msg_scrollbar','delta':text},
          {'type':'response.output_item.done','output_index':0,'item':message}, completed_event(0,[message])]
        payload = ''.join('data: '+json.dumps(e)+'\n\n' for e in events).encode()
        self.send_response(200); self.send_header('Content-Type','text/event-stream')
        self.send_header('Content-Length',str(len(payload))); self.send_header('Connection','close'); self.end_headers(); self.wfile.write(payload)
server = ThreadingHTTPServer(('127.0.0.1',0),Provider)
threading.Thread(target=server.serve_forever,daemon=True).start()
config = f'''data_dir = {json.dumps(str(ROOT / 'agent-data'))}
default_profile = "coding"
[profiles.coding]
model = "verify-scrollbar"
reasoning = "high"
system_prompt = "Synthetic scrollbar verification."
tools = []
approval = "auto"
[models.verify-scrollbar]
provider = "open_ai_responses"
model = "verify-scrollbar"
base_url = "http://127.0.0.1:{server.server_port}/v1"
api_key_env = "MINICORE_SCROLLBAR_DUMMY"
physical_context_window = 128000
output_budget_tokens = 16384
safety_margin_tokens = 1000
supported_reasoning = ["high"]
supports_tools = true
request_timeout_seconds = 30
'''
(ROOT / 'agent.toml').write_text(config)
def osa(script): return subprocess.check_output(['osascript','-e',script],text=True).rstrip('\n')
window = int(osa('tell application "iTerm2"\nactivate\nset w to (create window with default profile command "/bin/sh")\nreturn id of w\nend tell'))
def send(text):
    value = ' & '.join(f'(character id {ord(c)})' for c in text)
    osa(f'tell application "iTerm2" to tell current session of window id {window} to write text ({value}) newline NO')
def dimensions(width,height):
    osa(f'tell application "iTerm2" to tell current session of window id {window}\nset columns to {width}\nset rows to {height}\nset name to "MiniCore scrollbar verification"\nend tell')
def screen():
    rows=int(osa(f'tell application "iTerm2" to tell current session of window id {window} to get rows'))
    return '\n'.join(osa(f'tell application "iTerm2" to tell current session of window id {window} to get contents').splitlines()[-rows:])
def wait(predicate,label,seconds=15):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        text=screen()
        if predicate(text): return text
        time.sleep(.08)
    (OUT/'failure-screen.txt').write_text(text)
    raise AssertionError(label)
def capture(name):
    text=screen(); (OUT/f'{name}.txt').write_text(text); return text
def mouse(code,x,y,release=False): send(f'\x1b[<{code};{x};{y}{"m" if release else "M"}')
def bars(text): return any(c in text for c in '│┃█')
def positions(text): return {m.group():i for i,row in enumerate(text.splitlines()) for m in [re.search(r'SCROLL_ROW_\d{3}',row)] if m}
def shift(before,after,expected):
    a,b=positions(before),positions(after); common=a.keys() & b.keys()
    assert common, 'no shared rows'
    assert {b[key]-a[key] for key in common} == {expected}, (expected,a,b)
result={'window_id':window,'root':str(ROOT),'tui_sha256':hashlib.sha256(TUI.read_bytes()).hexdigest(),'agent_sha256':hashlib.sha256(AGENT.read_bytes()).hexdigest(),'checks':[],'pixel_parity':'not claimed; native text captures and injected SGR input'}
try:
    dimensions(80,24)
    command=f'cd {shlex.quote(str(WORK))}; stty -g > {ROOT}/tty-before; env MINICORE_SCROLLBAR_DUMMY=loopback-placeholder {shlex.quote(str(TUI))} --agent-bin {AGENT} --agent-config {ROOT}/agent.toml --workspace {WORK} --theme dark; rc=$?; stty -g > {ROOT}/tty-after; printf "\\nVERIFY_EXIT=%s\\n" "$rc"'
    send('/bin/sh -c '+shlex.quote(command)+'\r')
    wait(lambda s:'Create session' in s,'new session'); send('\t\t\t\t\t\r')
    wait(lambda s:'ready' in s and 'ctx' in s,'ready'); send('scrollbar verification\r')
    wait(lambda s:'SCROLL_ROW_119' in s and 'ready' in s,'settled output')
    time.sleep(1.2); before=capture('01-idle-hidden'); assert not bars(before)
    mouse(35,80,3); wait(lambda s:'█' in s and '│' in s,'hover active')
    hovered=capture('02-hover-active'); height=sum(row.endswith(('│','┃','█')) for row in hovered.splitlines())
    assert height>4, height
    mouse(35,79,3); wait(lambda s:'┃' in s and '█' not in s,'inactive thumb'); capture('03-inactive')
    time.sleep(1.2); assert not bars(capture('04-auto-hidden'))
    result['checks'].append('auto hide, hover activity, inactive glyph, full-height track')
    before=screen(); mouse(64,4,3); time.sleep(.2); after=capture('05-wheel-one'); shift(before,after,1)
    before=after; mouse(72,4,3); time.sleep(.2); after=capture('06-alt-wheel-five'); shift(before,after,5)
    before=after; send('\x1b[5~'); time.sleep(.2); after=capture('07-page-overlap'); shift(before,after,height-4)
    result['checks'].append('wheel 1, Alt-wheel 5, PageUp viewport minus 4')
    mouse(0,80,2); time.sleep(.15); start=capture('08-track-click-live')
    mouse(32,80,9); time.sleep(.15); dragged=capture('09-drag-live'); assert positions(start)!=positions(dragged)
    mouse(0,20,1,True); time.sleep(.15); released=capture('10-release-no-remap'); assert positions(dragged)==positions(released)
    result['checks'].append('track-center jump and live drag; release coordinates do not remap')
    mouse(0,80,2); mouse(32,80,8); time.sleep(.1); dimensions(100,30); time.sleep(.3)
    mouse(32,100,29); wait(lambda s:'SCROLL_ROW_119' in s,'resize drag to bottom'); capture('11-resize-drag')
    mouse(0,20,1,True); result['checks'].append('capture survives native resize and uses new geometry')
    send('\x03'); time.sleep(.15); send('\x03'); wait(lambda s:'VERIFY_EXIT=0' in s,'clean exit')
    assert (ROOT/'tty-before').read_text()==(ROOT/'tty-after').read_text(), 'TTY restoration mismatch'
    result['checks'].append('normal exit restores exact stty state')
    result['requests']=requests; assert len(requests)==1, requests
    result['status']='PASS'
except BaseException as error:
    result['status']='FAIL'; result['error']=repr(error)
    raise
finally:
    (OUT/'validation.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(result,ensure_ascii=False))
    server.shutdown(); server.server_close()
