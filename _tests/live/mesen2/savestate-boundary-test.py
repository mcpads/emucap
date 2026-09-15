#!/usr/bin/env python3
"""Check frozen state-I/O admission and post-load CPU execution on a managed SNES host.

Supply a ROM and an instruction-restorable checkpoint; neither is distributed with this test.
The output contains private runtime evidence. Only this test's launch generation is stopped.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket

from support import McpProcess, ROOT, default_binary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content", type=Path)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("--binary", type=Path, default=default_binary())
    parser.add_argument("--mcp-binary", type=Path, default=ROOT / "target/release/emucap-mcp")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--gameover", type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    content = args.content.resolve(strict=True)
    checkpoint = args.checkpoint.resolve(strict=True)
    original = checkpoint.read_bytes()
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env = os.environ.copy()
    env.update({
        "EMUCAP_EMU_HOME": str(output / "home"),
        "EMUCAP_PORT": str(port),
        "EMUCAP_REPO_ROOT": str(ROOT),
        "MESEN_BIN": str(args.binary.resolve(strict=True)),
    })
    mcp = McpProcess(args.mcp_binary.resolve(strict=True), env)
    launch_id = None
    records: list[dict] = []

    def call(name: str, arguments: dict | None = None, error: str | None = None) -> dict:
        arguments = arguments or {}
        response = mcp.request("tools/call", {"name": name, "arguments": arguments})
        records.append({"tool": name, "arguments": arguments, "response": response})
        (output / "requests.json").write_text(json.dumps(records, indent=2))
        result = response.get("result", {})
        if error is not None:
            assert result.get("isError"), f"{name} accepted an unsafe operation: {response}"
            assert error in json.dumps(result), response
            return result
        assert "error" not in response and not result.get("isError"), response
        return result["structuredContent"]

    def cpu() -> dict:
        return call("get_state", {"groups": ["cpu"]})["state"]

    def step(count: int, unit: str = "instructions") -> None:
        result = call("step", {"count": count, "unit": unit})
        assert result["status"] == "completed" and result["state"] == "frozen", result

    def load(path: Path = checkpoint) -> None:
        result = call("load_state", {"path": str(path)})
        assert result["status"] == "completed" and result["state"] == "frozen", result

    try:
        mcp.initialize()
        bootstrap = call("bootstrap")
        plan = call("launch_plan", {"content_path": str(content), "system": "snes"})
        assert plan["ready_to_launch"], plan
        launched = call("launch", {
            **plan["preferred_launcher"]["args"],
            "name": "savestate-boundary-test", "start_frozen": True, "display": False,
        })
        launch_id = launched["launch_id"]
        status = call("status")
        assert status["connected"] and status["state"] == "frozen", status
        assert status["contracts"]["state"] == "validated", status
        assert status["runtime_instance"]["launch_id"] == launch_id, status
        (output / "identity.json").write_text(json.dumps({
            "server_build": bootstrap["server_build"],
            "launch": launched,
            "rom_sha256": hashlib.sha256(content.read_bytes()).hexdigest(),
            "checkpoint_sha256": hashlib.sha256(original).hexdigest(),
        }, indent=2))

        load()
        restored = cpu()
        step(1)
        expected_first = cpu()
        step(180, "frames")
        call("screenshot", {"save_path": str(output / "cold.png")})

        # A PPU deadline can interrupt a memory read or the middle of an instruction. Both
        # operations must refuse before loading bytes or replacing an existing destination.
        before = cpu()
        protected = output / "protected.mss"
        protected.write_bytes(b"existing destination must survive unsafe save")
        protected_bytes = protected.read_bytes()
        call("save_state", {"path": str(protected), "snapshot_key": "unsafe-save"}, error="unsafe_halt")
        assert protected.read_bytes() == protected_bytes
        call("load_state", {"path": str(checkpoint)}, error="unsafe_halt")
        assert cpu() == before, "rejected frame-halt state I/O changed the CPU"

        capability = call("status")["recording_capability"]
        assert "state_load" not in capability["origins"] and "state_load" not in capability
        assert "next_frame_boundary" in capability["origins"]
        assert "reset_release" in capability["origins"] and "terminal_state" in capability

        # Explicitly finish the source instruction; this movement is caller-owned, not hidden
        # inside save/load. Restoration must now execute the same first instruction as cold start.
        step(1)
        load()
        assert cpu() == restored
        saved = output / "instruction.mss"
        saved_result = call("save_state", {"path": str(saved), "snapshot_key": "instruction-save"})
        assert saved_result["boundary"] == "instruction_boundary", saved_result
        assert cpu() == restored, "saving an instruction boundary advanced the CPU"
        step(1)
        assert cpu() == expected_first, "restored execution retained the old native continuation"
        load(saved)
        step(1)
        assert cpu() == expected_first, "newly saved state did not resume equivalently"

        invalid = output / "invalid.mss"
        invalid.write_bytes(b"not a Mesen state")
        call("load_state", {"path": str(invalid)}, error="io_error")
        status = call("status")
        assert status["connected"] and status["state"] == "frozen", status
        load()
        step(1)
        assert cpu() == expected_first, "failed-load recovery changed subsequent execution"
        step(180, "frames")
        call("screenshot", {"save_path": str(output / "warm.png")})

        if args.gameover:
            step(1)
            load(args.gameover.resolve(strict=True))
            step(180, "frames")
            call("screenshot", {"save_path": str(output / "gameover.png")})
            call("load_state", {"path": str(checkpoint)}, error="unsafe_halt")
            step(1)
            load()
            step(1)
            assert cpu() == expected_first, "game-over continuation leaked into the restored field"
            step(180, "frames")
            call("screenshot", {"save_path": str(output / "recovered180.png")})
            step(180, "frames")
            call("screenshot", {"save_path": str(output / "recovered360.png")})
            call("tap", {"buttons": ["left"], "press_frames": 10, "after_frames": 20})
            call("screenshot", {"save_path": str(output / "recovered-left.png")})

        call("reset")
        call("pause")
        step(1)
        load()
        step(1)
        assert cpu() == expected_first, "reset/load retained a pre-restore continuation"
        step(180, "frames")
        call("screenshot", {"save_path": str(output / "reset-restored180.png")})

        assert checkpoint.read_bytes() == original, "input checkpoint changed"
        (output / "passed.json").write_text(json.dumps({"passed": True, "cpu_first": expected_first}, indent=2))
        print("savestate boundary and restored execution checks passed", flush=True)
    finally:
        if launch_id:
            stopped = call("stop", {"launch_id": launch_id})
            assert stopped.get("stopped"), stopped
        mcp.close()


if __name__ == "__main__":
    main()
