"""Finite standard-library WebSocket peer for the REAL loopback phone server.

No Agent RPC connection. Kept phone projections never stand in for the TUI's
installed-canonical observer. Authentication material/content is never logged.
"""

import base64
import hashlib
import json
import os
import select
import socket
import struct
import time


class Phone:
    MAX_FRAME = 16 * 1024 * 1024

    def __init__(self, port, *, token=None, name="fixture-phone", receive_buffer=None, read_rate=None):
        self.read_rate = read_rate
        self.next_read = 0
        self.read_calls = self.throttled_polls = 0
        self.socket = socket.socket()
        if receive_buffer:
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, receive_buffer)
        self.socket.settimeout(5)
        self.socket.connect(("127.0.0.1", port))
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall((f"GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
                             f"Upgrade: websocket\r\nConnection: Upgrade\r\n"
                             f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = self.socket.recv(8192)
            if not chunk or len(data) + len(chunk) > 65536:
                raise AssertionError("invalid real-server WebSocket upgrade")
            data += chunk
        head, self.buffer = data.split(b"\r\n\r\n", 1)
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        if not head.startswith(b"HTTP/1.1 101") or accept.lower() not in head.lower():
            raise AssertionError("real-server WebSocket upgrade rejected")
        self.socket.setblocking(False)
        self.token = token
        self.state = None
        self.view_epoch = None
        self.blocks = []
        self.acks = {}
        self.closed = False
        self.paused = False
        self.snapshots = 0
        self.messages = 0
        self.bytes = 0
        self.seq = 0
        self.last_close = None
        self.send({"t": "hello", "v": 3, "token": token} if token else {
            "t": "login", "v": 3, "password": "isolated-phone-fixture", "device": name,
        })

    def frame(self, opcode, body):
        mask = os.urandom(4)
        size = len(body)
        prefix = bytes([0x80 | opcode])
        if size < 126:
            prefix += bytes([0x80 | size])
        elif size <= 65535:
            prefix += bytes([0x80 | 126]) + struct.pack("!H", size)
        else:
            prefix += bytes([0x80 | 127]) + struct.pack("!Q", size)
        payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(body))
        self.socket.settimeout(5)
        try:
            self.socket.sendall(prefix + mask + payload)
        finally:
            self.socket.setblocking(False)

    def send(self, value):
        self.frame(1, json.dumps(value, separators=(",", ":")).encode())

    def target(self):
        if self.state is None or not self.view_epoch:
            raise AssertionError("phone has no full projection target")
        session = self.state.get("session") or {}
        return {"view_epoch": self.view_epoch, "session_id": session.get("id"), "load": session.get("load")}

    def intent(self, kind, **fields):
        self.seq += 1
        operation = f"fixture-{self.seq}"
        self.send({"t": "intent", "id": operation, "kind": kind, **self.target(), **fields})
        return operation

    def read_ready(self):
        return not self.paused and not self.closed and time.monotonic() >= self.next_read

    def poll(self):
        if self.paused or self.closed:
            return
        if not self.read_ready():
            self.throttled_polls += 1
            return
        # A finite per-pump byte budget prevents a streaming phone starving PTY.
        for _ in range(1 if self.read_rate else 32):
            ready, _, _ = select.select([self.socket], [], [], 0)
            if not ready:
                break
            data = self.socket.recv(4096 if self.read_rate else 65536)
            self.read_calls += 1
            if not data:
                self.closed = True
                break
            self.bytes += len(data)
            if self.read_rate:
                self.next_read = time.monotonic() + len(data) / self.read_rate
            if self.bytes > 256 * 1024 * 1024:
                raise AssertionError("phone receive evidence budget exceeded")
            self.buffer += data
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            if not first & 0x80 or second & 0x80:
                raise AssertionError("unexpected fragmented/masked server frame")
            size = second & 127
            offset = 2
            if size == 126:
                if len(self.buffer) < 4:
                    return
                size, offset = struct.unpack("!H", self.buffer[2:4])[0], 4
            elif size == 127:
                if len(self.buffer) < 10:
                    return
                size, offset = struct.unpack("!Q", self.buffer[2:10])[0], 10
            if size > self.MAX_FRAME:
                raise AssertionError("phone frame exceeds fixture budget")
            if len(self.buffer) < offset + size:
                return
            body, self.buffer = self.buffer[offset:offset + size], self.buffer[offset + size:]
            opcode = first & 15
            if opcode == 8:
                self.closed = True
                self.last_close = "close_frame"
                return
            if opcode == 9:
                self.frame(10, body)
            elif opcode == 1:
                self.message(json.loads(body))
            elif opcode != 10:
                raise AssertionError("unexpected real-server frame kind")

    def message(self, row):
        self.messages += 1
        kind = row.get("t")
        if kind == "authorized":
            self.token = row["token"]
        elif kind == "welcome":
            if row["v"] != 3:
                raise AssertionError("phone protocol mismatch")
        elif kind == "snapshot":
            self.snapshots += 1
            self.state, self.blocks, self.view_epoch = row["state"], row["blocks"], row["view_epoch"]
        elif kind == "state":
            self.state = row["state"]
        elif kind == "blocks":
            self.blocks = self.blocks[:row["len"]]
            self.blocks.extend([None] * (row["len"] - len(self.blocks)))
            for update in row["upserts"]:
                index = update["i"]
                if "block" in update:
                    self.blocks[index] = update["block"]
                else:
                    block = self.blocks[index]
                    for field, value in update["append"].items():
                        if field in ("text", "reasoning"):
                            block[field] += value
                        elif field == "output":
                            if value:
                                block["output"][-1] += value[0]
                                block["output"].extend(value[1:])
                        else:
                            raise AssertionError("unknown phone append field")
        elif kind == "ack":
            if row["id"] in self.acks:
                raise AssertionError("duplicate phone operation ACK")
            if len(self.acks) >= 512:
                raise AssertionError("phone ACK fixture budget exceeded")
            self.acks[row["id"]] = row
        elif kind == "bye":
            self.last_close = row.get("reason")
        elif kind == "error":
            raise AssertionError("real phone server returned protocol error: " + row.get("code", "unknown"))

    def close(self):
        self.socket.close()
        self.closed = True
