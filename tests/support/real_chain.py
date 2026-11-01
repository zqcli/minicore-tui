#!/usr/bin/env python3
"""Basic real-TUI smoke cases using the shared PTY/provider driver."""

import json
import sys
import threading
import time

from process_support import run_bounded
from real_chain_load import Chain, Plan, STAGE_TIMEOUT, completed, delta, sse

PROMPT = "Exercise the isolated smoke fixture."
ANSWER = "SMOKEANSWEROK"
PARTIAL = "SMOKEPARTIALOK"


class SmokePlan(Plan):
    def __init__(self, case):
        tools = case in ("tool", "approval")
        super().__init__(tool_every=tools, tool_steps=1,
                         tool_name="write" if case == "approval" else "read",
                         tool_path="denied.txt" if case == "approval" else "fixture.txt")
        self.case = case
        self.request_limit = 2 if tools else 1
        self.allow_cancel_close = case == "cancel"
        self.cancel_release = threading.Event()
        self.register(ANSWER, self.request_limit - 1)

    def respond(self, handler, _index, _request):
        if self.case not in ("error", "cancel"):
            return False
        if self.case == "error":
            body = b'{"error":{"message":"fixture failure","type":"invalid_request_error"}}'
            handler.send_response(400)
            handler.send_header("Content-Type", "application/json")
        else:
            partial = sse([delta(PARTIAL)])
            body = partial + sse([completed()])
            handler.send_response(200)
            handler.send_header("Content-Type", "text/event-stream")
        handler.send_header("Content-Length", str(len(body)))
        handler.send_header("Connection", "close")
        handler.end_headers()
        handler.wfile.write(partial if self.case == "cancel" else body)
        handler.wfile.flush()
        if self.case == "cancel" and not self.cancel_release.wait(STAGE_TIMEOUT):
            raise AssertionError("cancel gate timed out")
        return True

    def close(self):
        self.cancel_release.set()


def main(binary, case):
    if case not in {"answer", "tool", "approval", "error", "cancel"}:
        raise ValueError("unknown smoke case")
    plan = SmokePlan(case)
    tools = '"read"' if case == "tool" else '"write"' if case == "approval" else ""
    started = time.monotonic()
    with Chain(binary, None, plan, tools=tools,
               approval="ask" if case == "approval" else "auto", screen_only=True) as chain:
        (chain.workspace / "fixture.txt").write_text("fixture read content", encoding="utf-8")
        chain.provider.gate.clear()
        chain.submit(PROMPT)
        chain.wait_for("provider request and busy display",
                       lambda: chain.provider.count() >= 1 and chain.screen.contains("Working"))
        if PROMPT not in json.dumps(chain.provider.request(0)):
            raise AssertionError("provider did not receive the fixture input")
        chain.provider.gate.set()
        if case == "cancel":
            chain.wait_for("streamed partial", lambda: chain.screen.contains(PARTIAL))
            chain.key(b"\x1b")
            chain.wait_for("cancel outcome", lambda: chain.screen.contains("interrupted"))
            if not chain.screen.contains(PARTIAL):
                raise AssertionError("cancelled stream lost its visible partial")
            plan.cancel_release.set()
        elif case == "approval":
            chain.wait_for("approval", lambda: chain.screen.contains("Allow once")
                           and chain.screen.contains("Deny"))
            chain.key(b"\x1b")
            chain.wait_for("denied tool continuation", lambda: chain.provider.count() >= 2)
            if (chain.workspace / "denied.txt").exists():
                raise AssertionError("denied tool wrote a file")
        if case == "tool":
            chain.wait_for("tool continuation", lambda: chain.provider.count() >= 2)
        if case == "error":
            chain.wait_for("provider failure", lambda: chain.screen.contains("failed")
                           and chain.screen.contains("400"))
        elif case != "cancel":
            chain.wait_for("answer and completion", lambda: chain.screen.contains(ANSWER)
                           and chain.screen.contains("completed"))
            if chain.screen.count(ANSWER) != 1:
                raise AssertionError("answer is duplicated on the terminal")
            if case in ("tool", "approval") and not chain.screen.contains("1 tool"):
                raise AssertionError("tool completion is not visible")
        if chain.provider.count() != plan.request_limit:
            raise AssertionError("unexpected provider request count")
        chain.shutdown()
        chain.provider.check()
    print(f"real-chain {case}: requests={plan.request_limit}, agent=stdio-child, "
          f"terminal=restored, exit=0, elapsed_ms={int((time.monotonic() - started) * 1000)}")


if __name__ == "__main__":
    run_bounded(lambda: main(sys.argv[1], sys.argv[2]), 75, f"real-chain {sys.argv[2]}")
