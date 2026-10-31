"""A gated stdio child for Client shutdown ownership, not canonical-chain evidence."""

import json
import socket
import sys
from pathlib import Path


control = socket.socket(socket.AF_UNIX)
control.settimeout(8)
control.connect(sys.argv[1])
shutdown = json.loads(sys.stdin.readline())
assert shutdown['method'] == 'agent.shutdown'
print(json.dumps({'jsonrpc': '2.0', 'id': shutdown['id'], 'result': {'shutdown': True}}), flush=True)
control.sendall(b'A')
fence = json.loads(sys.stdin.readline())
assert fence['method'] == 'test.fence'
print(json.dumps({'jsonrpc': '2.0', 'id': fence['id'], 'result': {'fenced': True}}), flush=True)
if control.recv(1) != b'R':
    sys.exit(1)
Path(sys.argv[2]).write_text('child exit owner finished\n')
control.close()
