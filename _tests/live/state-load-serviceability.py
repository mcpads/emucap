#!/usr/bin/env python3
"""Validate state-load acknowledgement and subsequent control in an isolated launch."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent / "mesen2"))
from support import McpProcess, ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content", type=Path)
    parser.add_argument("--system", choices=["psp", "pc98"], required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--group", required=True)
    parser.add_argument("--warmup", type=int, default=600)
    args = parser.parse_args()
    if not 1 <= args.warmup <= 5000:
        parser.error("warmup must be between 1 and 5000")
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, EMUCAP_EMU_HOME=str(out / "home"),
               EMUCAP_PORT=str(port), EMUCAP_REPO_ROOT=str(ROOT))
    mcp = McpProcess(ROOT / "target/release/emucap-mcp", env)
    records = []
    launch_id = None

    def call(name, parameters=None, expect_error=False):
        response = mcp.request("tools/call", {"name": name, "arguments": parameters or {}})
        records.append({"tool": name, "arguments": parameters or {}, "response": response})
        (out / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        failed = bool(response.get("error") or result.get("isError"))
        assert failed == expect_error, response
        return result.get("structuredContent", {})

    try:
        mcp.initialize()
        call("bootstrap")
        plan_args = {"content_path": str(args.content.resolve()), "system": args.system}
        if args.system == "pc98":
            plan_args["pc98_backend"] = "np2kai"
        plan = call("launch_plan", plan_args)
        assert plan["ready_to_launch"], plan
        launched = call("launch", {**plan["preferred_launcher"]["args"],
                                  "start_frozen": args.system == "pc98", "display": False, "sound": False,
                                  "name": "state-load-serviceability"})
        launch_id = launched["launch_id"]
        identity = call("status")
        (out / "identity.json").write_text(json.dumps({"launch": launched, "status": identity}, indent=2))
        call("pause")
        call("step", {"unit": "frames", "count": args.warmup})
        checkpoint = out / "checkpoint.state"
        call("save_state", {"path": str(checkpoint)})
        expected = call("get_state", {"groups": [args.group]})
        digest = hashlib.sha256(checkpoint.read_bytes()).hexdigest()
        for index in range(3):
            call("step", {"unit": "frames", "count": 30})
            if args.system == "psp":
                call("resume")
            loaded = call("load_state", {"path": str(checkpoint)})
            assert loaded["status"] == "completed" and loaded["state"] == "frozen", loaded
            status = call("status")
            assert status["connected"] and status["state"] == "frozen", status
            restored = call("get_state", {"groups": [args.group]})
            assert restored == expected, {"expected": expected, "restored": restored}
            call("step", {"unit": "instructions", "count": 1})
            call("get_state", {"groups": [args.group]})
            call("step", {"unit": "frames", "count": 2})
            if args.system == "pc98":
                call("screenshot", {"save_path": str(out / f"restored-{index}.png")})
        if args.system == "pc98":
            # Inject create failure only in this owned launch's staged firmware tree.
            candidates = list((out / "home").rglob("np2kai/bios.rom"))
            assert len(candidates) == 1, candidates
            temporary = candidates[0].parent / "temp_.sxx"
            assert not temporary.exists()
            temporary.mkdir()
            try:
                call("load_state", {"path": str(checkpoint)}, expect_error=True)
                status = call("status")
                assert status["connected"] and status["state"] == "frozen"
            finally:
                temporary.rmdir()
            call("load_state", {"path": str(checkpoint)})
            call("step", {"unit": "frames", "count": 2})
        assert hashlib.sha256(checkpoint.read_bytes()).hexdigest() == digest
        (out / "result.json").write_text(json.dumps({"passed": True, "checkpoint_sha256": digest}))
    finally:
        try:
            if launch_id:
                call("stop", {"launch_id": launch_id})
        finally:
            mcp.close()


if __name__ == "__main__":
    main()
