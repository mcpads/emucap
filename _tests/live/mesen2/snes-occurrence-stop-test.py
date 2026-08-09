#!/usr/bin/env python3
"""Prove filtered non-frame occurrence stop through the public Core path.

Usage: snes-occurrence-stop-test.py <bootable.sfc|smc> [mesen-binary]
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

from support import McpProcess, ROOT, default_binary


EVENT_CLASS = "snes_ppu_obj_consumption_read"
SELECTED_CLASSES = ("frame_boundary", "frame_completed", EVENT_CLASS)


def require_capability(status: dict) -> None:
    capability = status.get("recording_capability", {})
    advertised = {item.get("id"): item for item in capability.get("event_classes", [])}
    event = advertised.get(EVENT_CLASS, {})
    fields = {
        (item.get("kind"), item.get("path"))
        for item in event.get("filterable_fields", [])
    }
    if (
        capability.get("event_order") != "guest_emission"
        or not all(event_class in advertised for event_class in SELECTED_CLASSES)
        or not event.get("exact")
        or not event.get("stoppable")
        or fields != {("u64_range", "memory_kind"), ("u64_range", "address")}
    ):
        raise RuntimeError(f"filtered occurrence stop is not advertised: {capability}")


def recording_arguments(output_root: Path, occurrence: int) -> dict:
    return {
        "output_root": str(output_root),
        "origin": "reset_release",
        "frames": 10,
        "event_classes": list(SELECTED_CLASSES),
        "event_filters": [
            {
                "event_class": EVENT_CLASS,
                "terms": [
                    {
                        "kind": "u64_range",
                        "path": "memory_kind",
                        "start": 1,
                        "length": 1,
                    },
                    {
                        "kind": "u64_range",
                        "path": "address",
                        "start": 0,
                        "length": 65536,
                    },
                ],
            }
        ],
        "stop_on": {"event_class": EVENT_CLASS, "occurrence": occurrence},
    }


def validate_capture(bundle: Path, occurrence: int) -> dict:
    manifest = json.loads((bundle / "manifest.json").read_text())
    events = [
        json.loads(line)
        for line in (bundle / "events/segment-000.ndjson").read_text().splitlines()
    ]
    terminal = manifest.get("terminal", {})
    scope = manifest.get("scope", {})
    stop = terminal.get("stop_event", {})
    matching = [event for event in events if event.get("class") == EVENT_CLASS]
    if (
        terminal.get("operation_outcome") != "completed"
        or terminal.get("execution_outcome") != "event_stop"
        or terminal.get("integrity") != "complete"
        or terminal.get("publication") != "published"
        or terminal.get("final_execution_state") != "frozen"
    ):
        raise RuntimeError(f"occurrence capture terminal differs: {terminal}")
    if len(matching) != occurrence or events[-1] != matching[-1]:
        raise RuntimeError("the requested occurrence is not the final persisted record")
    expected_stop = {
        "sequence": matching[-1]["sequence"],
        "event_class": matching[-1]["class"],
        "clock_domain": matching[-1]["clock"]["domain"],
        "clock_tick": matching[-1]["clock"]["tick"],
        "frame": matching[-1]["frame"],
        "occurrence": occurrence,
    }
    if stop != expected_stop:
        raise RuntimeError(f"terminal and stream stop facts differ: {stop} != {expected_stop}")
    if (
        scope.get("f_end") != stop["frame"] + 1
        or terminal.get("final_frame") != stop["frame"]
        or manifest.get("counters", {}).get("dropped") != 0
        or manifest.get("loss", {}).get("truncated")
        or manifest.get("cleanup", {}).get("hooks") != "released"
        or manifest.get("cleanup", {}).get("sink") != "released"
    ):
        raise RuntimeError(f"partial-frame stop closure differs: {manifest}")
    if not all(
        event["payload"]["memory_kind"] == 1
        and 0 <= event["payload"]["address"] < 65536
        for event in matching
    ):
        raise RuntimeError(f"persisted event escaped the negotiated filter: {matching}")
    return {
        "events": len(events),
        "matching_events": len(matching),
        "stop_event": stop,
        "scope": scope,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("content")
    parser.add_argument("binary", nargs="?")
    parser.add_argument("--mcp-binary")
    parser.add_argument("--occurrence", type=int, default=2)
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
    if args.occurrence < 1:
        raise SystemExit("--occurrence must be positive")

    with tempfile.TemporaryDirectory(prefix="emucap-mesen-occurrence-") as temp:
        home = Path(temp)
        output_root = home / "bundles"
        output_root.mkdir()
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        env = os.environ.copy()
        env.update(
            {
                "EMUCAP_EMU_HOME": str(home),
                "EMUCAP_PORT": str(port),
                "EMUCAP_REPO_ROOT": str(ROOT),
                "EMUCAP_SESSION_ID": "mesen-occurrence-stop-test",
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
                    "name": "mesen-occurrence-stop-test",
                    "display": False,
                    "start_frozen": True,
                },
                timeout=60,
            )
            launch_id = launched["launch_id"]
            status = mcp.tool("status", {})
            require_capability(status)
            captured = mcp.tool(
                "debug",
                {
                    "operation": "record_window",
                    "known_capability_revision": status["capability_revision"],
                    "arguments": recording_arguments(output_root, args.occurrence),
                },
                timeout=90,
            )
            evidence = validate_capture(Path(captured["bundle_path"]), args.occurrence)
            final_status = mcp.tool("status", {})
            if final_status.get("state") != "frozen" or not final_status.get("connected"):
                raise RuntimeError(f"connection did not survive occurrence stop: {final_status}")
            print(
                json.dumps(
                    {
                        "ok": True,
                        "server_build": bootstrap.get("server_build"),
                        "host_build": final_status.get("host_build"),
                        "manifest_sha256": captured.get("manifest_sha256"),
                        "evidence": evidence,
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
