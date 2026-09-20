#!/usr/bin/env python3
"""Real PTY fold-transition regression; requires only the existing pyte dependency.

No draw boundaries are inferred from individual PTY reads. Screen samples follow
10 ms of output quiescence; raw placeholder detection additionally catches the
known old transition even when two draws arrive in one read. This is not proof
that every sub-10-ms terminal state was sampled. All Agent data is fixture-local.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import select
import time

from display_conversation_validation import (
    MARKERS, ModelServer, PtySession, create_fixture, save_screen, sha256,
)

ANCHORS = [MARKERS[key] for key in ("tool_user", "tool_before", "tool_after")]
RESULT = MARKERS["tool_result"]
PLACEHOLDER = b"Preparing conversation..."


def observe(session: PtySession, duration: float, evidence: dict | None = None) -> None:
    """Drain each ready burst before judging the parser's current screen."""
    deadline = time.monotonic() + duration
    tail = b""
    while time.monotonic() < deadline:
        offset = session.raw.tell()
        session.read_once(min(0.02, max(0, deadline - time.monotonic())))
        # A read may end halfway through a terminal update. Wait for/drain its
        # continuation before sampling; do not assert on a per-read screenshot.
        drain_deadline = time.monotonic() + 0.25
        while time.monotonic() < drain_deadline:
            ready, _, _ = select.select([session.master], [], [], 0.01)
            if not ready:
                break
            session.read_once(0)
        else:
            raise RuntimeError("PTY never became quiescent; screen sample inconclusive")
        end = session.raw.tell()
        if evidence is None or end == offset:
            continue
        with session.raw_path.open("rb") as raw:
            raw.seek(offset)
            data = raw.read(end - offset)
        combined = tail + data
        evidence["placeholder_occurrences"] += combined.count(PLACEHOLDER)
        tail = combined[-(len(PLACEHOLDER) - 1):]
        flow = session.screen.flow()
        counts = {anchor: flow.count(anchor) for anchor in ANCHORS}
        sample = {
            "raw_end": end, "anchor_counts": counts,
            "result_count": flow.count(RESULT),
            "parsed_placeholder": "Preparing conversation..." in flow,
        }
        evidence["samples"].append(sample)
        if any(count != 1 for count in counts.values()):
            evidence["anchor_losses"].append(sample)
            save_screen(session.raw_path.parent / f"anchor-loss-{end}.screen.txt", session.screen)


def header_hit(session: PtySession) -> tuple[int, int, str]:
    lines = session.screen.lines_text()
    candidates = [(row, line) for row, line in enumerate(lines)
                  if "read" in line.lower() and ("fixture.txt" in line
                      or (row + 1 < len(lines) and "fixture.txt" in lines[row + 1]))]
    if len(candidates) != 1:
        raise RuntimeError(f"expected one real read header, got {candidates!r}\n{session.screen.flow()}")
    row, line = candidates[0]
    column = line.lower().index("read")
    # pyte display indices are cells here: the fixture/header prefix has no
    # wide glyphs. The selected 'read' is actual content, not blank padding.
    return column + 1, row + 1, line.rstrip()


def run(binary: pathlib.Path, agent: pathlib.Path, output: pathlib.Path,
        cycles: int) -> dict:
    output.mkdir(parents=True)
    server = ModelServer(output)
    server.state.count = 2  # response 3 is the tool call; response 4 its answer
    server.start()
    session = None
    evidence = {"placeholder_occurrences": 0, "anchor_losses": [], "samples": []}
    try:
        workspace, _, config = create_fixture(output / "fixture", server.port)
        (workspace / "fixture.txt").write_text(
            RESULT + "\n" + "".join(f"fold fixture line {n}\n" for n in range(1, 8)),
            encoding="utf-8",
        )
        session = PtySession(binary, agent, config, workspace, output / "terminal.pty.raw", False)
        session.start()
        session.wait_for(["Create session"])
        session.send(b"\t\t\t\t\t\r")
        session.wait_for(["ctx ?", "ready"])
        session.send((MARKERS["tool_user"] + "\r").encode())
        session.wait_for(ANCHORS + ["ready"], 25)
        observe(session, 2.0)  # allow authoritative history hand-off to settle
        with server.state.lock:
            requests = list(server.state.requests)
            completed = list(server.state.completed_responses)
        if len(requests) != 2 or set(completed) != {3, 4}:
            raise RuntimeError(f"tool conversation incomplete: {requests!r}, {completed!r}")
        baseline = session.screen.flow()
        if any(baseline.count(anchor) != 1 for anchor in ANCHORS):
            raise RuntimeError(f"baseline anchors invalid\n{baseline}")
        if baseline.count(RESULT) not in (0, 1):
            raise RuntimeError("duplicate baseline tool result")
        expanded = RESULT in baseline
        save_screen(output / "baseline.screen.txt", session.screen)
        evidence["watched_raw_start"] = session.raw.tell()
        transitions = []
        for index in range(cycles * 2):
            column, row, header = header_hit(session)
            expanded = not expanded
            raw_start = session.raw.tell()
            # Real xterm SGR mouse protocol, not an AppEvent or key shortcut.
            session.send(f"\x1b[<0;{column};{row}M".encode())
            session.send(f"\x1b[<0;{column};{row}m".encode())
            observe(session, 0.8, evidence)  # also exceeds native 500-ms double-click window
            flow = session.screen.flow()
            actual = flow.count(RESULT)
            save_screen(output / f"toggle-{index + 1:02d}.screen.txt", session.screen)
            if actual != int(expanded) or any(flow.count(anchor) != 1 for anchor in ANCHORS):
                raise RuntimeError(f"click {index + 1} did not reach expected expanded={expanded}\n{flow}")
            transitions.append({"click": index + 1, "sgr_cell": [column, row],
                                "header": header, "expanded": expanded,
                                "tool_result_count": actual, "raw_start": raw_start,
                                "raw_end": session.raw.tell()})
        evidence["transitions"] = transitions
        evidence["visible_states"] = sum(item["expanded"] for item in transitions)
        evidence["hidden_states"] = len(transitions) - evidence["visible_states"]
        if not evidence["samples"] or session.screen.alt_enters < 1:
            raise RuntimeError("missing parsed-screen/alternate-screen evidence")
        evidence["flash_detected"] = bool(evidence["placeholder_occurrences"] or evidence["anchor_losses"]
                                           or any(s["parsed_placeholder"] for s in evidence["samples"]))
        evidence["requests"] = requests
        evidence["completed_responses"] = completed
        evidence["binary"] = {"path": str(binary), "sha256": sha256(binary)}
        status = session.shutdown()
        if status != 0:
            raise RuntimeError(f"TUI exited {status}")
        evidence["exit"] = status
        return evidence
    finally:
        if session is not None:
            if session.pid is not None:
                save_screen(output / "last.screen.txt", session.screen)
                session.abort()
        server.close()
        (output / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tui-bin", type=pathlib.Path)
    parser.add_argument("--old-tui-bin", type=pathlib.Path,
                        help="optional fold-negative control (b844cf5, NOT message-bug 7fea27e)")
    parser.add_argument("--agent-bin", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--cycles", type=int, default=3)
    args = parser.parse_args()
    if not (args.tui_bin or args.old_tui_bin) or args.cycles < 1:
        parser.error("provide a TUI binary and at least one cycle")
    for path in (args.tui_bin, args.old_tui_bin, args.agent_bin):
        if path and (not path.is_file() or not os.access(path, os.X_OK)):
            parser.error(f"executable missing: {path}")
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error(f"output must be empty: {output}")
    output.mkdir(parents=True, exist_ok=True)
    summary = {"schema": "fold-display-v1", "agent_sha256": sha256(args.agent_bin),
               "parser": "existing pyte alternate-screen helper; 10-ms quiescent samples",
               "limitations": "No terminal draw delimiter; sub-10-ms blank states may be missed. "
                              "Raw placeholder bytes detect the known old flash even if samples coalesce."}
    ok = True
    for name, binary, expected_flash in (("current", args.tui_bin, False),
                                         ("old-negative", args.old_tui_bin, True)):
        if binary is None:
            continue
        try:
            result = run(binary.resolve(), args.agent_bin.resolve(), output / name, args.cycles)
            passed = result["flash_detected"] == expected_flash
            summary[name] = {"passed": passed, "flash_detected": result["flash_detected"],
                             "placeholder_occurrences": result["placeholder_occurrences"],
                             "anchor_loss_samples": len(result["anchor_losses"]),
                             "visible_states": result["visible_states"],
                             "hidden_states": result["hidden_states"], "binary": result["binary"]}
            ok &= passed
        except Exception as error:
            # Setup/toggle failures are NEVER accepted as a negative control.
            summary[name] = {"passed": False, "invalid_run": str(error)}
            ok = False
    summary["passed"] = ok
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
