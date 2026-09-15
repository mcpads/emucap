#!/usr/bin/env python3
"""Verify that PSX state restoration remains usable from frame and debugger halts.

Owned disc/checkpoint inputs and all output are private. Only this test's launch is stopped.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "mesen2"))
from support import McpProcess, ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content", type=Path)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--binary",
        type=Path,
        default=ROOT / "adapters/mednafen/work/mednafen/src/mednafen",
    )
    parser.add_argument("--firmware", type=Path)
    parser.add_argument("--write-address", type=lambda s: int(s, 0), required=True)
    parser.add_argument("--button", default="circle")
    parser.add_argument(
        "--media-member",
        action="append",
        default=[],
        help="Reviewed relative media member; repeat for every launch-plan member",
    )
    parser.add_argument(
        "--mode", choices=["breakpoint", "instruction", "frame"], default="instruction"
    )
    parser.add_argument("--instruction-count", type=int, default=50000)
    parser.add_argument("--repeats", type=int, default=1)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    checkpoint = args.checkpoint.resolve(strict=True)
    if args.instruction_count <= 0 or args.repeats <= 0:
        parser.error("instruction-count and repeats must be positive")
    original = hashlib.sha256(checkpoint.read_bytes()).hexdigest()
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    env = os.environ.copy()
    env.update(
        EMUCAP_EMU_HOME=str(out / "home"),
        EMUCAP_PORT=str(port),
        EMUCAP_REPO_ROOT=str(ROOT),
        MEDNAFEN_BIN=str(args.binary.resolve(strict=True)),
    )
    if args.firmware:
        env["EMUCAP_MEDNAFEN_FIRMWARE"] = str(args.firmware.resolve(strict=True))
    mcp = McpProcess(ROOT / "target/release/emucap-mcp", env)
    launch_id = None
    records = []

    def call(name, arguments=None):
        response = mcp.request(
            "tools/call", {"name": name, "arguments": arguments or {}}
        )
        records.append(
            {"tool": name, "arguments": arguments or {}, "response": response}
        )
        (out / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        if "error" in response or result.get("isError"):
            raise RuntimeError(f"{name}: {response}")
        return result["structuredContent"]

    def load():
        return call("load_state", {"path": str(checkpoint)})

    def arm():
        return call(
            "set_breakpoint",
            {
                "kind": "write",
                "memory_type": "cpu",
                "start": args.write_address,
                "end": args.write_address + 3,
                "pause_on_hit": True,
            },
        )["id"]

    def tap():
        return call(
            "tap", {"buttons": [args.button], "press_frames": 6, "after_frames": 120}
        )

    try:
        mcp.initialize()
        boot = call("bootstrap")
        plan = call(
            "launch_plan",
            {"content_path": str(args.content.resolve(strict=True)), "system": "psx"},
        )
        if (
            not plan["ready_to_launch"]
            and plan.get("indirect_media", {}).get("state") == "review_required"
        ):
            follow = plan["next_action"]["then_call"]
            approved = follow["arguments"]["indirect_media_approval"]["members"]
            assert sorted(approved) == sorted(args.media_member), (
                f"Review media members first: {approved}"
            )
            plan = call(follow["tool"], follow["arguments"])
        assert plan["ready_to_launch"], plan
        launched = call(
            "launch",
            {
                **plan["preferred_launcher"]["args"],
                "start_frozen": True,
                "display": False,
                "sound": False,
                "name": "psx-restore-test",
            },
        )
        launch_id = launched["launch_id"]
        status = call("status")
        (out / "identity.json").write_text(
            json.dumps(
                {
                    "bootstrap": boot,
                    "launch": launched,
                    "status": status,
                    "checkpoint_sha256": original,
                },
                indent=2,
            )
        )
        load()
        cold = call("get_state", {"groups": ["g0"]})["state"]
        call("step", {"count": 1, "unit": "instructions"})
        cold_next = call("get_state", {"groups": ["g0"]})["state"]
        # Two fields replace both halves of an interlaced presentation. A single field
        # can legitimately retain pixels from the destination's previous surface.
        load()
        call("step", {"count": 2, "unit": "frames"})
        call("screenshot", {"save_path": str(out / "cold-frame.png")})
        for iteration in range(args.repeats):
            print("restore iteration", iteration, flush=True)
            load()
            if args.mode in ("instruction", "breakpoint"):
                remaining = args.instruction_count
                while remaining:
                    count = min(remaining, 5000)
                    call("step", {"count": count, "unit": "instructions"})
                    remaining -= count
                if args.mode == "breakpoint":
                    pc = call("get_state", {"groups": ["g0"]})["state"]["g0.NPC"]
                    bp = call(
                        "set_breakpoint",
                        {
                            "kind": "exec",
                            "memory_type": "cpu",
                            "start": pc,
                            "end": pc,
                            "pause_on_hit": True,
                        },
                    )["id"]
                    interrupted = call("step", {"count": 10, "unit": "instructions"})
                    assert interrupted.get("status") == "interrupted", interrupted
                    call("poll_events")
                    call("clear_breakpoint", {"id": bp})
            else:
                call("step", {"count": 6, "unit": "frames"})
            call("get_state", {"groups": ["g0"]})
            load()
            assert call("get_state", {"groups": ["g0"]})["state"] == cold
            bp = arm()
            restored = tap()
            call("poll_events")
            assert call("status")["connected"]
            call("get_state", {"groups": ["g0"]})
            call("clear_breakpoint", {"id": bp})
            call("step", {"count": 120, "unit": "frames"})
        call("screenshot", {"save_path": str(out / "resumed.png")})
        call("step", {"count": 5000, "unit": "instructions"})
        load()
        call("step", {"count": 1, "unit": "instructions"})
        assert call("get_state", {"groups": ["g0"]})["state"] == cold_next
        load()
        remaining = args.instruction_count
        while remaining:
            count = min(remaining, 5000)
            call("step", {"count": count, "unit": "instructions"})
            remaining -= count
        load()
        call("step", {"count": 2, "unit": "frames"})
        call("screenshot", {"save_path": str(out / "warm-frame.png")})
        assert (out / "cold-frame.png").read_bytes() == (
            out / "warm-frame.png"
        ).read_bytes(), "Restored two-field presentation differs from cold load"
        assert hashlib.sha256(checkpoint.read_bytes()).hexdigest() == original
        (out / "passed.json").write_text(
            json.dumps(
                {"passed": True, "mode": args.mode, "restored_tap": restored}, indent=2
            )
        )
        print("PSX restore passed:", args.mode, flush=True)
    finally:
        try:
            if launch_id:
                call("stop", {"launch_id": launch_id})
        finally:
            mcp.close()


if __name__ == "__main__":
    main()
