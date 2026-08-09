#!/usr/bin/env python3
"""Prove controlled entry and opt-in repeatability across two cold Mesen launches."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

from support import McpProcess, ROOT, default_binary


PROFILE = "mesen_snes_repeatable"
PROFILE_CONDITIONS = "b9f4760915a13576fe4fa5c55a75dffd0e79987ac6259cea1bff5a1701826d6b"


def free_port() -> int:
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    return port


def require_tool_error(mcp: McpProcess, name: str, arguments: dict) -> dict:
    response = mcp.request(
        "tools/call", {"name": name, "arguments": arguments}, timeout=30
    )
    result = response.get("result", {})
    if "error" in response or not result.get("isError"):
        raise RuntimeError(f"MCP {name} unexpectedly succeeded: {response}")
    structured = result.get("structuredContent")
    if not isinstance(structured, dict):
        raise RuntimeError(f"MCP {name} returned no structured error: {response}")
    return structured


def record_window_call(status: dict, arguments: dict) -> dict:
    revision = status.get("capability_revision")
    if not isinstance(revision, str) or not revision:
        raise RuntimeError(f"status has no capability revision: {status}")
    return {
        "operation": "record_window",
        "known_capability_revision": revision,
        "arguments": arguments,
    }


def require_repeatable_status(status: dict) -> None:
    if status.get("state") != "frozen":
        raise RuntimeError(f"controlled launch did not return frozen: {status}")
    start = status.get("launch_start", {})
    if not start.get("controlled") or start.get("boundary") != "pre_first_instruction":
        raise RuntimeError(f"controlled entry boundary is absent: {start}")
    repeatability = status.get("recording_capability", {}).get("repeatability", {})
    if (
        repeatability.get("profile") != PROFILE
        or repeatability.get("conditions_sha256") != PROFILE_CONDITIONS
        or repeatability.get("origins") != ["reset_release"]
        or not repeatability.get("requires_input_movie")
    ):
        raise RuntimeError(f"repeatability capability differs: {repeatability}")


def require_frozen_wall_delay(mcp: McpProcess, seconds: float) -> dict:
    before = mcp.tool("status", {})
    time.sleep(seconds)
    after = mcp.tool("status", {})
    if before.get("state") != "frozen" or after.get("state") != "frozen":
        raise RuntimeError(f"wall delay escaped the frozen entry: {before} -> {after}")
    if before.get("frame") != after.get("frame"):
        raise RuntimeError(
            "wall-clock delay became guest time at a controlled entry: "
            f"{before.get('frame')} -> {after.get('frame')}"
        )
    return after


def normalized_evidence(bundle: Path) -> dict:
    manifest = json.loads((bundle / "manifest.json").read_text())
    terminal = manifest.get("terminal", {})
    if (
        terminal.get("operation_outcome") != "completed"
        or terminal.get("integrity") != "complete"
    ):
        raise RuntimeError(f"recording did not complete with integrity: {terminal}")
    events = [
        json.loads(line)
        for line in (bundle / "events/segment-000.ndjson").read_text().splitlines()
    ]
    if not events:
        raise RuntimeError("repeatability recording contained no events")
    f_start = manifest["scope"]["f_start"]
    clock_origins: dict[str, int] = {}
    normalized_events = []
    for event in events:
        clock = event["clock"]
        domain = clock["domain"]
        clock_origins.setdefault(domain, clock["tick"])
        normalized_events.append(
            {
                "sequence": event["sequence"],
                "class": event["class"],
                "contract_sha256": event["contract_sha256"],
                "frame": event["frame"] - f_start,
                "clock": {
                    "domain": domain,
                    "tick": clock["tick"] - clock_origins[domain],
                },
                "payload": event["payload"],
            }
        )
    normalized_bytes = json.dumps(
        normalized_events, sort_keys=True, separators=(",", ":")
    ).encode()
    content_members = {
        (member["role"], member["path"]): member["sha256"]
        for member in manifest["members"]
        if member["role"] in {"input_movie", "initial_snapshot"}
    }
    if {role for role, _ in content_members} != {"input_movie", "initial_snapshot"}:
        raise RuntimeError(f"repeatability content members are incomplete: {content_members}")
    return {
        "events": normalized_events,
        "normalized_events_sha256": hashlib.sha256(normalized_bytes).hexdigest(),
        "content_member_sha256": content_members,
        "event_member_sha256": next(
            member["sha256"]
            for member in manifest["members"]
            if member["role"] == "events"
        ),
    }


def capture_once(
    mcp: McpProcess,
    rom: Path,
    output_root: Path,
    movie: Path,
    name: str,
    wall_delay: float,
) -> tuple[dict, dict]:
    launched = mcp.tool(
        "launch",
        {
            "content_path": str(rom),
            "system": "snes",
            "name": name,
            "display": False,
            "execution_profile": "repeatable",
        },
        timeout=60,
    )
    if not launched.get("launched") or not launched.get("connected"):
        raise RuntimeError(f"repeatable launch failed: {launched}")
    launch_id = launched["launch_id"]
    identity = launched.get("runtime_instance", {}).get("execution_profile", {})
    if identity != {"id": PROFILE, "conditions_sha256": PROFILE_CONDITIONS}:
        raise RuntimeError(f"runtime execution identity differs: {identity}")
    if not launched.get("runtime_instance", {}).get("start_frozen"):
        raise RuntimeError(f"runtime did not persist controlled entry: {launched}")

    try:
        status = mcp.tool("status", {})
        require_repeatable_status(status)
        status = require_frozen_wall_delay(mcp, wall_delay)
        entry_frame = status.get("frame")

        children_before = set(output_root.iterdir())
        require_tool_error(
            mcp,
            "debug",
            record_window_call(
                status,
                {
                    "output_root": str(output_root),
                    "frames": 2,
                    "origin": "next_frame_boundary",
                    "require_repeatable": True,
                },
            ),
        )
        require_tool_error(
            mcp,
            "debug",
            record_window_call(
                status,
                {
                    "output_root": str(output_root),
                    "frames": 2,
                    "origin": "reset_release",
                    "require_repeatable": True,
                },
            ),
        )
        if set(output_root.iterdir()) != children_before:
            raise RuntimeError("a refused repeatability request staged an output")
        after_refusal = mcp.tool("status", {})
        if after_refusal.get("state") != "frozen" or after_refusal.get("frame") != entry_frame:
            raise RuntimeError(f"a refused repeatability request changed guest time: {after_refusal}")

        captured = mcp.tool(
            "debug",
            record_window_call(
                status,
                {
                    "output_root": str(output_root),
                    "frames": 1,
                    "origin": "reset_release",
                    "input_path": str(movie),
                    "event_classes": ["frame_boundary", "snes_cpu_instruction"],
                    "start_on": {"event_class": "snes_cpu_instruction"},
                    "initial_snapshots": [
                        {
                            "label": "wram-head",
                            "memory_type": "snesWorkRam",
                            "address": 0,
                            "length": 256,
                        }
                    ],
                    "require_repeatable": True,
                },
            ),
            timeout=90,
        )
        evidence = normalized_evidence(Path(captured["bundle_path"]))
        terminal_status = mcp.tool("status", {})
        if terminal_status.get("state") != "frozen":
            raise RuntimeError(f"recording did not finish frozen: {terminal_status}")
        return evidence, {
            "launch_id": launch_id,
            "entry_frame": entry_frame,
            "terminal_frame": terminal_status.get("frame"),
            "manifest_sha256": captured.get("manifest_sha256"),
        }
    finally:
        mcp.tool("stop", {"launch_id": launch_id}, timeout=20)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rom", required=True)
    parser.add_argument("--binary")
    parser.add_argument("--mcp-binary")
    parser.add_argument("--wall-delay", type=float, default=0.75)
    args = parser.parse_args()
    rom = Path(args.rom).resolve()
    binary = Path(args.binary).resolve() if args.binary else default_binary().resolve()
    mcp_binary = (
        Path(args.mcp_binary).resolve()
        if args.mcp_binary
        else (ROOT / "target/release/emucap-mcp").resolve()
    )
    for label, path in (("ROM", rom), ("Mesen", binary), ("release MCP", mcp_binary)):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-repeatable-") as temp:
        home = Path(temp)
        output_root = home / "bundles"
        output_root.mkdir()
        movie = home / "empty.movie"
        movie.write_text("0:\n")
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "EMUCAP_PORT": str(free_port()),
                "EMUCAP_REPO_ROOT": str(ROOT),
                "EMUCAP_SESSION_ID": "mesen-controlled-repeatable-test",
                "MESEN_BIN": str(binary),
            }
        )
        mcp = McpProcess(mcp_binary, env)
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

            first, first_run = capture_once(
                mcp, rom, output_root, movie, "repeatable-cold-a", args.wall_delay
            )
            second, second_run = capture_once(
                mcp, rom, output_root, movie, "repeatable-cold-b", args.wall_delay
            )
            if first["events"] != second["events"]:
                raise RuntimeError("normalized guest event payloads or order differed")
            if first["content_member_sha256"] != second["content_member_sha256"]:
                raise RuntimeError("guest-derived content member hashes differed")

            default = mcp.tool(
                "launch",
                {
                    "content_path": str(rom),
                    "system": "snes",
                    "name": "default-running-regression",
                    "display": False,
                },
                timeout=60,
            )
            default_id = default["launch_id"]
            try:
                default_status = mcp.tool("status", {})
                if default.get("state") != "running" or default_status.get("state") != "running":
                    raise RuntimeError(f"default launch stopped returning running: {default}")
                if default_status.get("recording_capability", {}).get("repeatability") is not None:
                    raise RuntimeError("default launch advertised opt-in repeatability")
            finally:
                mcp.tool("stop", {"launch_id": default_id}, timeout=20)

            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": bootstrap.get("server_build"),
                        "profile": PROFILE,
                        "conditions_sha256": PROFILE_CONDITIONS,
                        "normalized_events_sha256": first["normalized_events_sha256"],
                        "exact_event_members_equal": (
                            first["event_member_sha256"] == second["event_member_sha256"]
                        ),
                        "content_member_sha256": {
                            f"{role}:{path}": digest
                            for (role, path), digest in first[
                                "content_member_sha256"
                            ].items()
                        },
                        "runs": [first_run, second_run],
                        "default_launch_state": "running",
                    },
                    separators=(",", ":"),
                )
            )
            return 0
        finally:
            mcp.close()


if __name__ == "__main__":
    raise SystemExit(main())
