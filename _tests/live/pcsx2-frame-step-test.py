#!/usr/bin/env python3
"""Check long frame advances and debugger interruption on a managed PCSX2 host.

Supply owned game media and BIOS. Output contains private state and captures.
Use a short output path on Unix: the session-owned PINE socket is below it.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent / "mesen2"))
from support import McpProcess, ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content", type=Path)
    parser.add_argument("--bios", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = os.environ.copy()
    env.update(EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_EMU_HOME=str(out / "home"),
               EMUCAP_PORT=str(port), EMUCAP_PCSX2_BIOS=str(args.bios.resolve(strict=True)))
    mcp = McpProcess(ROOT / "target/release/emucap-mcp", env)
    records = []
    launch_id = None

    def call(name, arguments=None):
        response = mcp.request("tools/call", {"name": name, "arguments": arguments or {}}, timeout=290)
        records.append({"tool": name, "arguments": arguments, "response": response})
        (out / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        assert "error" not in response and not result.get("isError"), response
        return result["structuredContent"]

    try:
        mcp.initialize()
        call("bootstrap")
        plan = call("launch_plan", {"content_path": str(args.content.resolve(strict=True)), "system": "ps2"})
        assert plan["ready_to_launch"], plan
        launched = call("launch", {**plan["preferred_launcher"]["args"], "name": "pcsx2-frame-step-test", "display": False})
        launch_id = launched["launch_id"]
        deadline = time.monotonic() + 30
        while not call("status")["connected"]:
            assert time.monotonic() < deadline
            time.sleep(0.1)
        call("pause")
        for count in [16, 180, 5000]:
            result = call("step", {"count": count, "unit": "frames"})
            assert result["status"] == "completed" and result["advanced"] == count and result["state"] == "frozen", result
            print("completed", count, flush=True)
        call("screenshot", {"save_path": str(out / "after-long-step.png")})
        checkpoint = out / "base.p2s"
        call("save_state", {"path": str(checkpoint)})
        debug = call("debug", {"operation": "describe"})
        probed = call("debug", {"operation": "probe", "known_capability_revision": debug["capability_revision"],
            "arguments": {"state": str(checkpoint), "frame": 120, "memory_type": "ee", "address": 0, "length": 4}})
        assert probed["status"] == "completed" and probed["completed_frames"] == 120, probed
        state = call("get_state", {"groups": ["cpu"]})
        pc = state["state"]["cpu.pc"]
        bp = call("set_breakpoint", {"kind": "exec", "memory_type": "ee", "start": pc, "end": pc, "pause_on_hit": True})
        result = call("step", {"count": 5000, "unit": "frames"})
        assert result["status"] == "interrupted" and result["advanced"] < 5000 and result["state"] == "frozen", result
        events = call("poll_events")
        assert events["events"], events
        short = call("step", {"count": 16, "unit": "frames"})
        assert short["status"] == "interrupted" and short["advanced"] < 16, short
        call("clear_breakpoint", {"id": bp["id"]})
        # Resume must not auto-pause when an interrupted request's old counter expires.
        call("resume")
        time.sleep(1)
        assert call("status")["state"] == "running"
        call("pause")
        # A new request also starts with its own frame budget.
        result = call("step", {"count": 16, "unit": "frames"})
        assert result["status"] == "completed" and result["advanced"] == 16, result
        (out / "passed.json").write_text(json.dumps({"passed": True, "breakpoint_pc": pc, "events": events}, indent=2))
        print("PCSX2 long step and interruption checks passed", flush=True)
    finally:
        if launch_id:
            assert call("stop", {"launch_id": launch_id})["stopped"]
        mcp.close()


if __name__ == "__main__":
    main()
