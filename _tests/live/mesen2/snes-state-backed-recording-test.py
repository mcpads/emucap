#!/usr/bin/env python3
"""Prove producer-managed state-backed windows through the public Core path.

Usage: snes-state-backed-recording-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

from support import McpProcess, ROOT, default_binary


def validate_capability(status: dict) -> None:
    capability = status.get("recording_capability", {})
    state_load = capability.get("state_load", {})
    if (
        "state_load" not in capability.get("origins", [])
        or state_load.get("format") != "mesen-savestate"
        or state_load.get("alignment") != "restored_frame_boundary"
        or state_load.get("requires_input_movie") is not True
    ):
        raise RuntimeError(f"state-backed recording is not advertised: {capability}")


def validate_bundle(bundle: Path, receipt: dict, frames: int) -> list[dict]:
    manifest = json.loads((bundle / "manifest.json").read_text())
    event_bytes = (bundle / "events/segment-000.ndjson").read_bytes()
    events = [json.loads(line) for line in event_bytes.splitlines()]
    state_bytes = (bundle / "initial.state").read_bytes()
    expected_start = receipt["frozen"]["frame"]
    if manifest.get("request", {}).get("initial_state") != receipt:
        raise RuntimeError("manifest does not bind the exact producer receipt")
    if manifest.get("scope", {}).get("origin") != "state_load":
        raise RuntimeError(f"state origin differs: {manifest.get('scope')}")
    if (
        manifest.get("scope", {}).get("f_start") != expected_start
        or manifest.get("scope", {}).get("f_end") != expected_start + frames
        or manifest.get("terminal", {}).get("integrity") != "complete"
        or manifest.get("terminal", {}).get("final_execution_state") != "frozen"
        or [event.get("frame") for event in events]
        != list(range(expected_start, expected_start + frames))
    ):
        raise RuntimeError(f"state-backed window differs: {manifest}, {events}")
    if (
        len(state_bytes) != receipt["snapshot"]["bytes"]
        or hashlib.sha256(state_bytes).hexdigest() != receipt["snapshot"]["sha256"]
    ):
        raise RuntimeError("bundle initial.state does not match the producer receipt")
    initial_members = [
        member
        for member in manifest.get("members", [])
        if member.get("role") == "initial_state"
    ]
    if len(initial_members) != 1 or initial_members[0].get("sha256") != receipt["snapshot"]["sha256"]:
        raise RuntimeError(f"initial state member descriptor differs: {initial_members}")
    return events


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--mcp-binary")
    parser.add_argument("--frames", type=int, default=3)
    args = parser.parse_args()
    content = Path(args.content).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    mcp_binary = (
        Path(args.mcp_binary).resolve()
        if args.mcp_binary
        else (ROOT / "target/release/emucap-mcp").resolve()
    )
    if not content.is_file() or not binary.is_file() or not mcp_binary.is_file():
        raise SystemExit("content, compatible Mesen, and release MCP binaries must exist")
    if args.frames < 1:
        raise SystemExit("--frames must be positive")

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-state-window-") as temp:
        home = Path(temp)
        output_root = home / "bundles"
        output_root.mkdir()
        state_path = home / "saved-state.mss"
        movie_path = home / "neutral.movie"
        movie_path.write_text("".join(f"{frame}:\n" for frame in range(args.frames)))
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "EMUCAP_PORT": str(port),
                "EMUCAP_REPO_ROOT": str(ROOT),
                "EMUCAP_SESSION_ID": "mesen-state-backed-recording-test",
                "MESEN_BIN": str(binary),
            }
        )
        mcp = McpProcess(mcp_binary, env)
        launch_id: str | None = None
        try:
            mcp.initialize()
            bootstrap = mcp.tool("bootstrap", {})
            source_revision = subprocess.check_output(
                ["git", "rev-parse", "--short=7", "HEAD"], cwd=ROOT, text=True
            ).strip()
            if bootstrap.get("server_build") != source_revision:
                raise RuntimeError(
                    "release MCP does not identify the current committed source: "
                    f"server={bootstrap.get('server_build')} source={source_revision}"
                )
            launched = mcp.tool(
                "launch",
                {
                    "content_path": str(content),
                    "system": "snes",
                    "name": "mesen-state-backed-recording-test",
                    "display": False,
                    "start_frozen": True,
                },
                timeout=60,
            )
            launch_id = launched["launch_id"]
            mcp.tool("step", {"count": 1, "unit": "frames"})
            status = mcp.tool("status", {})
            validate_capability(status)
            saved = mcp.tool(
                "save_state",
                {"path": str(state_path), "preserve_for_recording": True},
            )
            receipt = saved.get("snapshot_receipt", {})
            if (
                not receipt.get("snapshot_id", "").startswith("snapshot-")
                or receipt.get("frozen", {}).get("boundary") != "frame_boundary"
                or receipt.get("source", {}).get("launch_id") != launch_id
            ):
                raise RuntimeError(f"save_state receipt differs: {saved}")
            record_arguments = {
                "output_root": str(output_root),
                "origin": "state_load",
                "frames": args.frames,
                "input_path": str(movie_path),
                "initial_state": {"snapshot_id": receipt["snapshot_id"]},
            }
            captures = []
            streams = []
            for _ in range(2):
                captured = mcp.tool(
                    "debug",
                    {
                        "operation": "record_window",
                        "known_capability_revision": status["capability_revision"],
                        "arguments": record_arguments,
                    },
                    timeout=90,
                )
                captures.append(captured)
                streams.append(
                    validate_bundle(Path(captured["bundle_path"]), receipt, args.frames)
                )
            if streams[0] != streams[1]:
                raise RuntimeError("two windows from one receipt have different event streams")
            final_status = mcp.tool("status", {})
            if final_status.get("state") != "frozen" or not final_status.get("connected"):
                raise RuntimeError(f"connection did not survive state windows: {final_status}")
            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": bootstrap.get("server_build"),
                        "host_build": final_status.get("host_build"),
                        "snapshot_id": receipt["snapshot_id"],
                        "frame": receipt["frozen"]["frame"],
                        "frames": args.frames,
                        "manifest_sha256": [
                            capture.get("manifest_sha256") for capture in captures
                        ],
                    },
                    separators=(",", ":"),
                )
            )
            return 0
        finally:
            if launch_id is not None:
                try:
                    mcp.tool("stop", {"launch_id": launch_id}, timeout=20)
                except Exception:
                    pass
            mcp.close()


if __name__ == "__main__":
    raise SystemExit(main())
