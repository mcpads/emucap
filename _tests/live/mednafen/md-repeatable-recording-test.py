#!/usr/bin/env python3
"""Prove that repeatable MD recording restores its producer-owned initial state."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


LIVE_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(LIVE_ROOT / "mesen2"))

from support import McpProcess, ROOT  # noqa: E402


PROFILE = "mednafen_md_repeatable"
PROFILE_CONDITIONS = "c2e0f5529d92090521831109ff2e757f48140234c2f145651c790f0e41ef6d69"


def free_port() -> int:
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    return port


def record_window_call(status: dict, output_root: Path, movie: Path) -> dict:
    revision = status.get("capability_revision")
    if not isinstance(revision, str) or not revision:
        raise RuntimeError(f"status has no capability revision: {status}")
    return {
        "operation": "record_window",
        "known_capability_revision": revision,
        "arguments": {
            "output_root": str(output_root),
            "frames": 2,
            "origin": "reset_release",
            "input_path": str(movie),
            "event_classes": ["frame_boundary", "frame_completed"],
            "require_repeatable": True,
        },
    }


def require_repeatable_status(status: dict) -> None:
    if status.get("state") != "frozen":
        raise RuntimeError(f"controlled launch did not return frozen: {status}")
    start = status.get("launch_start", {})
    if not start.get("controlled") or start.get("boundary") != "pre_first_instruction":
        raise RuntimeError(f"controlled entry boundary is absent: {start}")
    repeatability = status.get("recording_capability", {}).get("repeatability", {})
    expected = {
        "profile": PROFILE,
        "conditions_sha256": PROFILE_CONDITIONS,
        "origins": ["reset_release"],
        "requires_input_movie": True,
    }
    if repeatability != expected:
        raise RuntimeError(f"repeatability capability differs: {repeatability}")


def normalized_events(bundle: Path) -> tuple[list[dict], str]:
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
    f_start = manifest["scope"]["f_start"]
    clock_origins: dict[str, int] = {}
    normalized = []
    for event in events:
        clock = event["clock"]
        domain = clock["domain"]
        clock_origins.setdefault(domain, clock["tick"])
        normalized.append(
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
    encoded = json.dumps(normalized, sort_keys=True, separators=(",", ":")).encode()
    return normalized, hashlib.sha256(encoded).hexdigest()


def read_ram(mcp: McpProcess) -> str:
    result = mcp.tool(
        "read_memory", {"memory_type": "ram", "address": 0, "length": 256}
    )
    value = result.get("hex")
    if not isinstance(value, str) or len(value) != 512:
        raise RuntimeError(f"unexpected MD RAM response: {result}")
    return value


def mutate_ram(mcp: McpProcess, current: str) -> str:
    replacement = f"{int(current[:2], 16) ^ 0xFF:02x}"
    mcp.tool(
        "write_memory",
        {"memory_type": "ram", "address": 0, "hex": replacement},
    )
    changed = read_ram(mcp)
    if changed[:2].lower() != replacement:
        raise RuntimeError("the deliberate between-recording RAM mutation did not land")
    return replacement


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rom", required=True)
    parser.add_argument("--binary")
    parser.add_argument("--mcp-binary")
    parser.add_argument("--wall-delay", type=float, default=0.75)
    args = parser.parse_args()

    rom = Path(args.rom).resolve()
    binary = (
        Path(args.binary).resolve()
        if args.binary
        else (ROOT / "adapters/mednafen/work/mednafen/src/mednafen").resolve()
    )
    mcp_binary = (
        Path(args.mcp_binary).resolve()
        if args.mcp_binary
        else (ROOT / "target/release/emucap-mcp").resolve()
    )
    for label, path in (("ROM", rom), ("Mednafen", binary), ("release MCP", mcp_binary)):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    with tempfile.TemporaryDirectory(prefix="emucap-mednafen-md-repeatable-") as temp:
        home = Path(temp)
        output_root = home / "bundles"
        output_root.mkdir()
        movie = home / "empty.movie"
        movie.write_text("0:\n1:\n")
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "EMUCAP_PORT": str(free_port()),
                "EMUCAP_REPO_ROOT": str(ROOT),
                "EMUCAP_SESSION_ID": "mednafen-md-repeatable-test",
                "MEDNAFEN_BIN": str(binary),
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

            launched = mcp.tool(
                "launch",
                {
                    "content_path": str(rom),
                    "system": "md",
                    "name": "md-repeatable-state-restore",
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

            try:
                status = mcp.tool("status", {})
                require_repeatable_status(status)
                entry_frame = status.get("frame")
                time.sleep(args.wall_delay)
                delayed = mcp.tool("status", {})
                if delayed.get("state") != "frozen" or delayed.get("frame") != entry_frame:
                    raise RuntimeError(f"wall delay became guest time: {status} -> {delayed}")

                first = mcp.tool(
                    "debug", record_window_call(delayed, output_root, movie), timeout=90
                )
                first_events, first_event_hash = normalized_events(Path(first["bundle_path"]))
                first_ram = read_ram(mcp)
                replacement = mutate_ram(mcp, first_ram)

                status = mcp.tool("status", {})
                second = mcp.tool(
                    "debug", record_window_call(status, output_root, movie), timeout=90
                )
                second_events, second_event_hash = normalized_events(Path(second["bundle_path"]))
                second_ram = read_ram(mcp)
                if first_events != second_events:
                    raise RuntimeError("normalized recording events differed after state restore")
                if first_ram != second_ram:
                    raise RuntimeError("post-window RAM differed after repeatable state restore")
                terminal = mcp.tool("status", {})
                if terminal.get("state") != "frozen":
                    raise RuntimeError(f"recording did not finish frozen: {terminal}")
            finally:
                mcp.tool("stop", {"launch_id": launch_id}, timeout=20)

            ordinary = mcp.tool(
                "launch",
                {
                    "content_path": str(rom),
                    "system": "md",
                    "name": "md-ordinary-regression",
                    "display": False,
                },
                timeout=60,
            )
            ordinary_id = ordinary["launch_id"]
            try:
                ordinary_status = mcp.tool("status", {})
                if ordinary.get("state") != "running" or ordinary_status.get("state") != "running":
                    raise RuntimeError(f"ordinary launch stopped returning running: {ordinary}")
                if ordinary_status.get("recording_capability", {}).get("repeatability") is not None:
                    raise RuntimeError("ordinary launch advertised opt-in repeatability")
            finally:
                mcp.tool("stop", {"launch_id": ordinary_id}, timeout=20)

            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": bootstrap.get("server_build"),
                        "profile": PROFILE,
                        "conditions_sha256": PROFILE_CONDITIONS,
                        "normalized_events_sha256": first_event_hash,
                        "event_hashes_equal": first_event_hash == second_event_hash,
                        "post_window_ram_sha256": hashlib.sha256(bytes.fromhex(first_ram)).hexdigest(),
                        "between_recording_mutation": replacement,
                        "terminal_state": "frozen",
                        "ordinary_launch_state": "running",
                    },
                    separators=(",", ":"),
                )
            )
            return 0
        finally:
            mcp.close()


if __name__ == "__main__":
    raise SystemExit(main())
