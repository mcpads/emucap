#!/usr/bin/env python3
"""Compare restored CPU execution and presentation across destination halt offsets.

Caller-owned inputs and generated checkpoints stay in a private output directory.
This diagnostic reports differences; it does not assume every differing register is a bug.
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
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("content", type=Path)
    p.add_argument("--system", required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--checkpoint", type=Path)
    p.add_argument("--warmup", type=int, default=120)
    p.add_argument("--save-offset", type=int, default=0)
    p.add_argument("--require-equal", action="store_true")
    p.add_argument("--offsets", default="0,1,1000,5000,50000")
    p.add_argument("--media-member", action="append", default=[])
    p.add_argument("--firmware", type=Path)
    p.add_argument("--group", default="g0")
    a = p.parse_args()
    try:
        offsets = [int(value) for value in a.offsets.split(",")]
    except ValueError:
        p.error("offsets must be comma-separated instruction counts")
    if a.save_offset < 0 or any(value < 0 for value in offsets) or not 1 <= a.warmup <= 5000:
        p.error("offsets must be nonnegative and warmup must be between 1 and 5000")
    out = a.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    env = os.environ.copy()
    env.update(
        EMUCAP_EMU_HOME=str(out / "home"),
        EMUCAP_PORT=str(port),
        EMUCAP_REPO_ROOT=str(ROOT),
    )
    if a.firmware:
        env["EMUCAP_MEDNAFEN_FIRMWARE"] = str(a.firmware.resolve())
    m = McpProcess(ROOT / "target/release/emucap-mcp", env)
    records = []
    launch_id = None

    def call(name, args=None):
        response = m.request("tools/call", {"name": name, "arguments": args or {}})
        records.append({"tool": name, "arguments": args or {}, "response": response})
        (out / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        if response.get("error") or result.get("isError"):
            raise RuntimeError(f"{name}: {response}")
        return result["structuredContent"]

    checkpoint = a.checkpoint.resolve() if a.checkpoint else out / "checkpoint.mcs"

    def load():
        return call("load_state", {"path": str(checkpoint)})

    def advance(count):
        while count:
            n = min(5000, count)
            call("step", {"count": n, "unit": "instructions"})
            count -= n

    def observe(label):
        load()
        before = call("get_state", {"groups": [a.group]})
        advance(1)
        after = call("get_state", {"groups": [a.group]})
        load()
        call("step", {"count": 2, "unit": "frames"})
        image = out / (label + ".png")
        call("screenshot", {"save_path": str(image)})
        state = call("status")
        assert state["connected"] and state["state"] == "frozen"
        return {
            "before": before,
            "after_one": after,
            "image_sha256": hashlib.sha256(image.read_bytes()).hexdigest(),
        }

    try:
        m.initialize()
        call("bootstrap")
        plan = call(
            "launch_plan",
            {"content_path": str(a.content.resolve()), "system": a.system},
        )
        if (
            not plan["ready_to_launch"]
            and plan.get("indirect_media", {}).get("state") == "review_required"
        ):
            follow = plan["next_action"]["then_call"]
            members = follow["arguments"]["indirect_media_approval"]["members"]
            assert sorted(members) == sorted(a.media_member), members
            plan = call(follow["tool"], follow["arguments"])
        assert plan["ready_to_launch"], plan
        launched = call(
            "launch",
            {
                **plan["preferred_launcher"]["args"],
                "start_frozen": True,
                "display": False,
                "sound": False,
                "name": "state-clock-audit",
            },
        )
        launch_id = launched["launch_id"]
        status = call("status")
        (out / "identity.json").write_text(
            json.dumps({"launch": launched, "status": status}, indent=2)
        )
        if not a.checkpoint:
            call("step", {"count": a.warmup, "unit": "frames"})
            advance(a.save_offset)
            call("save_state", {"path": str(checkpoint)})
        digest = hashlib.sha256(checkpoint.read_bytes()).hexdigest()
        baseline = observe("baseline")
        rows = []
        for count in offsets:
            load()
            advance(count)
            observed = observe("offset-" + str(count))
            row = {"offset": count, "equal": observed == baseline, "observed": observed}
            rows.append(row)
            (out / "comparison.json").write_text(
                json.dumps(
                    {"checkpoint_sha256": digest, "baseline": baseline, "cases": rows},
                    indent=2,
                )
            )
            print(count, row["equal"], flush=True)
        assert hashlib.sha256(checkpoint.read_bytes()).hexdigest() == digest
        if a.require_equal:
            assert all(row["equal"] for row in rows), "restored execution or presentation differs"
    finally:
        try:
            if launch_id:
                call("stop", {"launch_id": launch_id})
        finally:
            m.close()


if __name__ == "__main__":
    main()
